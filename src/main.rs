mod app;
mod data;
mod input;
mod log;
mod paths;
mod spell;
mod tz;
mod ui;

use std::io;
use std::time::Duration;
use anyhow::Result;
use crossterm::{
    event::{self, Event},
    terminal,
    ExecutableCommand,
};
use ratatui::{backend::CrosstermBackend, Terminal};
use app::{App, Overlay};

fn main() -> Result<()> {
    // Install a panic hook that restores the terminal before printing the panic.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().execute(terminal::LeaveAlternateScreen);
        original_hook(info);
    }));

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(terminal::EnterAlternateScreen)?;
    let backend  = CrosstermBackend::new(stdout);
    let mut term = Terminal::new(backend)?;
    term.clear()?;

    let result = run(&mut term);

    terminal::disable_raw_mode()?;
    io::stdout().execute(terminal::LeaveAlternateScreen)?;

    result
}

fn run(term: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    let mut app = App::new()?;

    loop {
        term.draw(|frame| ui::render(frame, &mut app))?;

        // Handle a pending $EDITOR request before polling for keyboard events.
        if let Some(pe) = app.pending_editor.take() {
            let tmppath = std::env::temp_dir().join(format!("ptm_{}.md", pe.slot));
            match suspend_and_edit(term, &pe.content, &tmppath)? {
                Some(content) => app.handle_editor_result(pe.target, content)?,
                None => {
                    app.overlay = Some(Overlay::Warn {
                        title: " Edit Conflict ".into(),
                        body: format!(
                            "A temp file for this record already exists.\n\
                             Another ptm session may be editing it, or a\n\
                             previous session crashed.\n\n\
                             File: {}\n\n\
                             Delete the file to resume editing.",
                            tmppath.display()
                        ),
                    });
                }
            }
            continue; // redraw immediately
        }

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key) => app.handle_key(key)?,
                Event::Resize(_, _) => { /* ratatui handles resize automatically */ }
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

/// Suspend ratatui, open `$EDITOR` with `content` written to `tmppath`, then
/// resume ratatui and return the saved text (trailing newlines stripped).
///
/// Returns `Ok(None)` without touching the terminal if `tmppath` already
/// exists — the caller should show a conflict warning to the user.
/// Uses `O_CREAT | O_EXCL` so the existence check and file creation are atomic.
fn suspend_and_edit(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    content: &str,
    tmppath: &std::path::Path,
) -> Result<Option<String>> {
    use std::io::Write as _;

    // Atomically create the file. Returns None (conflict) if it already exists.
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmppath)
    {
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
        Err(e) => return Err(e.into()),
        Ok(mut f) => f.write_all(content.as_bytes())?,
    }

    // Temporarily return the terminal to a normal state.
    terminal::disable_raw_mode()?;
    io::stdout().execute(terminal::LeaveAlternateScreen)?;

    // Launch editor.
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    std::process::Command::new(&editor).arg(tmppath).status()?;

    // Read the result.
    let result = std::fs::read_to_string(tmppath).unwrap_or_else(|e| {
        crate::log::warn(&format!(
            "suspend_and_edit: failed to read temp file {}: {e} — edit discarded",
            tmppath.display()
        ));
        String::new()
    });
    if let Err(e) = std::fs::remove_file(tmppath) {
        crate::log::warn(&format!(
            "suspend_and_edit: failed to remove temp file {}: {e}",
            tmppath.display()
        ));
    }

    // Resume ratatui.
    terminal::enable_raw_mode()?;
    io::stdout().execute(terminal::EnterAlternateScreen)?;
    term.clear()?;

    Ok(Some(result.trim_end_matches('\n').to_string()))
}

#[cfg(test)]
mod docs_tests {
    //! Guards the `AGENTS.md` repository tree against drift (finding #22).
    //!
    //! The documented tree had grown a file that does not exist
    //! (`ui/views/reference_library.rs`) and lost one that does (`tz.rs`), and
    //! described `input/mod.rs` as a placeholder when it is fully implemented.
    //! A layout section that lies is worse than none: it is the map a future
    //! session navigates by.  This test parses the tree out of `AGENTS.md` and
    //! compares it with what is on disk, so the next drift fails the build
    //! instead of misdirecting someone.

    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};

    /// Parse the fenced tree block under `## Repository Layout`.
    ///
    /// Returns the set of paths it names (relative to the repo root) that look
    /// like files — i.e. that carry an extension or are `main.rs`-style leaves.
    /// Directories are skipped: they are implied by their children, and the
    /// tree draws them with a trailing `/`.
    fn documented_files() -> BTreeSet<PathBuf> {
        let agents = include_str!("../AGENTS.md");
        let start = agents
            .find("## Repository Layout")
            .expect("AGENTS.md must have a 'Repository Layout' section");
        let section = &agents[start..];
        let open = section.find("```").expect("the layout section must have a code block") + 3;
        let close = section[open..]
            .find("```")
            .expect("the layout code block must be closed");
        let block = &section[open..open + close];

        let mut stack: Vec<String> = Vec::new();
        let mut files = BTreeSet::new();
        for line in block.lines() {
            // A tree row looks like `│   ├── name.rs    — description`; the
            // indentation before the connector gives the depth.
            let Some(conn_pos) = line.find("── ") else { continue };
            let prefix = &line[..conn_pos];
            if !prefix.chars().all(|c| c == '│' || c == ' ' || c == '├' || c == '└') {
                continue;
            }
            // Four columns per level; the connector itself is the last level's
            // marker, so the depth is the indentation divided by four.
            let depth = prefix.len() / 4;
            let rest = &line[conn_pos + "── ".len()..];
            let name = rest.split_whitespace().next().unwrap_or("");
            if name.is_empty() {
                continue;
            }
            stack.truncate(depth);
            if name.ends_with('/') {
                stack.push(name.trim_end_matches('/').to_string());
            } else {
                let mut path = PathBuf::from("src");
                for part in &stack {
                    path.push(part);
                }
                path.push(name);
                files.insert(path);
            }
        }
        files
    }

    /// Every `.rs` file under `root`, as paths relative to the repo root (so
    /// they compare directly with [`documented_files`]).
    fn actual_files(root: &Path, dir: &Path, out: &mut BTreeSet<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                actual_files(root, &path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                if let Ok(rel) = path.strip_prefix(root) {
                    out.insert(rel.to_path_buf());
                }
            }
        }
    }

    #[test]
    fn agents_md_tree_matches_the_source_tree() {
        let documented = documented_files();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut actual = BTreeSet::new();
        actual_files(root, &root.join("src"), &mut actual);

        let missing: Vec<_> = documented.difference(&actual).collect();
        let undocumented: Vec<_> = actual.difference(&documented).collect();

        assert!(
            missing.is_empty(),
            "AGENTS.md documents files that do not exist:\n{:#?}",
            missing
        );
        assert!(
            undocumented.is_empty(),
            "AGENTS.md does not document these files:\n{:#?}",
            undocumented
        );
    }
}
