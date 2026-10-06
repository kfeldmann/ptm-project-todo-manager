mod app;
mod data;
mod input;
mod log;
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
