//! md — Markdown file viewer and spell-checker.
//!
//! Usage: md <file.md>
//!
//! Keybindings
//! ───────────
//!   j / k           Scroll one line down / up
//!   Ctrl-F / Ctrl-B  Scroll half-page down / up
//!   g               Jump to top
//!   G               Jump to bottom
//!   /               Begin incremental search (highlight matches)
//!   Enter           Confirm search and return to normal mode
//!   ESC             Cancel search / clear active search
//!   e               Edit file in $EDITOR, reload on return
//!   S               Spell-check file, write corrections back
//!   ?               Show key-binding help
//!   Q               Quit

// ─── Module declarations ──────────────────────────────────────────────────────

mod log; // ./log.rs

/// Shared spell-checker (path-included from the main ptm source).
/// All `crate::log::warn` calls inside it resolve to this crate's `log` module.
#[path = "../../spell.rs"]
mod spell;

/// UI helpers — the `ui` module hierarchy must mirror the ptm layout so that
/// `markdown.rs` can resolve `use crate::ui::theme::Theme;` correctly.
mod ui;

// ─── Imports ──────────────────────────────────────────────────────────────────

use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal, ExecutableCommand,
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

use ui::theme::Theme;

// ─── Overlay ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Overlay {
    SpellCheck {
        segments:    Vec<(String, bool)>,
        bad_words:   Vec<usize>,
        current:     usize,
        suggestions: Vec<String>,
        done:        bool,
    },
    Warn {
        title: String,
        body:  String,
    },
    KeyHelp {
        scroll: u16,
    },
}

// ─── App state ────────────────────────────────────────────────────────────────

struct App {
    filepath:     PathBuf,
    content:      String,
    should_quit:  bool,

    // Scroll position (line index into the rendered output).
    scroll:       usize,
    /// Published by `render`; read by `handle_key` to clamp scroll.
    max_scroll:   std::cell::Cell<usize>,
    /// Published by `render`; used by half-page scroll calculations.
    view_height:  std::cell::Cell<usize>,

    // Incremental search.
    search:       String,
    searching:    bool,

    // Overlays.
    overlay:      Option<Overlay>,

    // Set to `true` to trigger $EDITOR suspension in the event loop.
    pending_editor: bool,

    // Spell-checker (None when no dictionary is found).
    spell:        Option<spell::SpellChecker>,
}

impl App {
    fn new(filepath: PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(&filepath)?;
        let data_dir = user_data_dir();
        let _ = std::fs::create_dir_all(&data_dir);
        let spell = spell::SpellChecker::new(&data_dir);
        Ok(App {
            filepath,
            content,
            should_quit: false,
            scroll: 0,
            max_scroll:  std::cell::Cell::new(0),
            view_height: std::cell::Cell::new(24),
            search:    String::new(),
            searching: false,
            overlay:   None,
            pending_editor: false,
            spell,
        })
    }

    fn reload_file(&mut self) {
        match std::fs::read_to_string(&self.filepath) {
            Ok(content) => self.content = content,
            Err(e) => crate::log::warn(&format!("reload_file: {e}")),
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let max = self.max_scroll.get();
        self.scroll = (self.scroll + n).min(max);
    }

    fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }
}

fn user_data_dir() -> PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".to_string()))
                .join(".local")
                .join("share")
        });
    base.join("md")
}

// ─── Entry point ──────────────────────────────────────────────────────────────

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 || args[1] == "--help" || args[1] == "-h" {
        eprintln!("Usage: md <file.md>");
        std::process::exit(1);
    }
    let filepath = PathBuf::from(&args[1]);
    if !filepath.exists() {
        eprintln!("md: file not found: {}", filepath.display());
        std::process::exit(1);
    }

    // Restore terminal before panicking.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = io::stdout().execute(terminal::LeaveAlternateScreen);
        original_hook(info);
    }));

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    stdout.execute(terminal::EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut term  = Terminal::new(backend)?;
    term.clear()?;

    let result = run(&mut term, filepath);

    terminal::disable_raw_mode()?;
    io::stdout().execute(terminal::LeaveAlternateScreen)?;
    result
}

fn run(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    filepath: PathBuf,
) -> Result<()> {
    let mut app = App::new(filepath)?;

    loop {
        term.draw(|frame| render(frame, &app))?;

        // Suspend TUI and launch $EDITOR when requested, then reload.
        if app.pending_editor {
            app.pending_editor = false;
            suspend_and_edit(term, &app.filepath.clone())?;
            app.reload_file();
            // Clamp scroll to the new document length.
            let max = app.max_scroll.get();
            app.scroll = app.scroll.min(max);
            continue;
        }

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(key)    => handle_key(&mut app, key)?,
                Event::Resize(..)  => { /* ratatui handles resize; next draw re-renders */ }
                _ => {}
            }
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// Temporarily suspend ratatui, open `$EDITOR` on `path`, then resume.
fn suspend_and_edit(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    path: &std::path::Path,
) -> Result<()> {
    terminal::disable_raw_mode()?;
    io::stdout().execute(terminal::LeaveAlternateScreen)?;

    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    std::process::Command::new(&editor).arg(path).status()?;

    terminal::enable_raw_mode()?;
    io::stdout().execute(terminal::EnterAlternateScreen)?;
    term.clear()?;
    Ok(())
}

// ─── Key handling ─────────────────────────────────────────────────────────────

fn handle_key(app: &mut App, key: crossterm::event::KeyEvent) -> Result<()> {
    // Overlay absorbs all keys first.
    if app.overlay.is_some() {
        handle_key_overlay(app, key)?;
        return Ok(());
    }

    // Search mode: most keys feed the query string.
    if app.searching {
        match key.code {
            KeyCode::Enter => {
                app.searching = false;
            }
            KeyCode::Esc => {
                app.searching = false;
                app.search.clear();
            }
            KeyCode::Backspace => {
                app.search.pop();
            }
            KeyCode::Char(c)
                if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                app.search.push(c);
            }
            _ => {}
        }
        return Ok(());
    }

    // Normal mode.
    match key.code {
        // ── Quit ──────────────────────────────────────────────────────────
        KeyCode::Char('Q') => app.should_quit = true,

        // ── Clear search ──────────────────────────────────────────────────
        KeyCode::Esc if !app.search.is_empty() => app.search.clear(),

        // ── Scrolling ─────────────────────────────────────────────────────
        KeyCode::Char('j') | KeyCode::Down  => app.scroll_down(1),
        KeyCode::Char('k') | KeyCode::Up    => app.scroll_up(1),

        // Half-page via Ctrl+F / Ctrl+B (vi style).
        KeyCode::Char('f')
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            let half = (app.view_height.get() / 2).max(1);
            app.scroll_down(half);
        }
        KeyCode::Char('b')
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            let half = (app.view_height.get() / 2).max(1);
            app.scroll_up(half);
        }
        KeyCode::PageDown => {
            let half = (app.view_height.get() / 2).max(1);
            app.scroll_down(half);
        }
        KeyCode::PageUp => {
            let half = (app.view_height.get() / 2).max(1);
            app.scroll_up(half);
        }

        // Go to top / bottom.
        KeyCode::Char('g') | KeyCode::Home => app.scroll = 0,
        KeyCode::Char('G') | KeyCode::End  => app.scroll = app.max_scroll.get(),

        // ── Search ────────────────────────────────────────────────────────
        KeyCode::Char('/') => {
            app.searching = true;
            app.search.clear();
        }

        // ── Editor ────────────────────────────────────────────────────────
        KeyCode::Char('e') => app.pending_editor = true,

        // ── Spell check ───────────────────────────────────────────────────
        KeyCode::Char('S') => open_spell_check(app)?,

        // ── Key help ──────────────────────────────────────────────────────
        KeyCode::Char('?') => {
            app.overlay = Some(Overlay::KeyHelp { scroll: 0 });
        }

        _ => {}
    }
    Ok(())
}

// ─── Overlay key handling ─────────────────────────────────────────────────────

fn handle_key_overlay(app: &mut App, key: crossterm::event::KeyEvent) -> Result<()> {
    let overlay = match app.overlay.take() {
        Some(o) => o,
        None    => return Ok(()),
    };

    app.overlay = match overlay {
        Overlay::SpellCheck { segments, bad_words, current, suggestions, done } => {
            handle_spell_key(app, key, segments, bad_words, current, suggestions, done)?
        }
        Overlay::Warn { .. } => {
            // Any key dismisses.
            None
        }
        Overlay::KeyHelp { mut scroll } => {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => None,
                KeyCode::Char('j') | KeyCode::Down => {
                    scroll = scroll.saturating_add(1);
                    Some(Overlay::KeyHelp { scroll })
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    scroll = scroll.saturating_sub(1);
                    Some(Overlay::KeyHelp { scroll })
                }
                _ => Some(Overlay::KeyHelp { scroll }),
            }
        }
    };
    Ok(())
}

// ─── Spell check logic ────────────────────────────────────────────────────────

fn open_spell_check(app: &mut App) -> Result<()> {
    if app.content.trim().is_empty() {
        return Ok(());
    }
    let Some(checker) = app.spell.as_ref() else {
        app.overlay = Some(Overlay::Warn {
            title: " Spell Check Unavailable ".into(),
            body:  "No spelling dictionary found.\n\
                    To enable spell check, install a dictionary:\n\
                    \n\
                    apt install hunspell-en-us\n\
                    \n\
                    Or set PTM_DICT_DIR to a directory containing\n\
                    en_US.aff and en_US.dic.".into(),
        });
        return Ok(());
    };
    let segments  = spell::tokenize(&app.content);
    let bad_words: Vec<usize> = segments
        .iter()
        .enumerate()
        .filter(|(_, (w, is_word))| *is_word && !checker.check(w))
        .map(|(i, _)| i)
        .collect();
    let done = bad_words.is_empty();
    let suggestions = if done {
        Vec::new()
    } else {
        checker.suggest(&segments[bad_words[0]].0)
    };
    app.overlay = Some(Overlay::SpellCheck {
        segments, bad_words, current: 0, suggestions, done,
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn handle_spell_key(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    mut segments:    Vec<(String, bool)>,
    mut bad_words:   Vec<usize>,
    mut current:     usize,
    mut suggestions: Vec<String>,
    done:            bool,
) -> Result<Option<Overlay>> {
    if done {
        return Ok(None);
    }

    match key.code {
        // Close and save.
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
            save_spell_result(app, &segments);
            return Ok(None);
        }

        // Replace once with suggestion N (1–9).
        KeyCode::Char(c) if matches!(c, '1'..='9') => {
            let n = (c as u8 - b'1') as usize;
            if n < suggestions.len() {
                segments[bad_words[current]].0 = suggestions[n].clone();
                bad_words.remove(current);
            } else {
                return Ok(Some(Overlay::SpellCheck {
                    segments, bad_words, current, suggestions, done: false,
                }));
            }
        }

        // Skip this occurrence.
        KeyCode::Char('s') => { current += 1; }

        // Accept all occurrences for this session.
        KeyCode::Char('a') => {
            let word = segments[bad_words[current]].0.clone();
            let to_remove: std::collections::HashSet<usize> = bad_words[current..]
                .iter()
                .filter(|&&idx| segments[idx].0 == word)
                .copied()
                .collect();
            bad_words.retain(|idx| !to_remove.contains(idx));
        }

        // Add to personal dictionary.
        KeyCode::Char('i') => {
            let word = segments[bad_words[current]].0.clone();
            if let Some(ref mut checker) = app.spell {
                checker.add_word(&word);
            }
            let to_remove: std::collections::HashSet<usize> = bad_words[current..]
                .iter()
                .filter(|&&idx| segments[idx].0 == word)
                .copied()
                .collect();
            bad_words.retain(|idx| !to_remove.contains(idx));
        }

        _ => {
            return Ok(Some(Overlay::SpellCheck {
                segments, bad_words, current, suggestions, done: false,
            }));
        }
    }

    if current >= bad_words.len() {
        save_spell_result(app, &segments);
        return Ok(Some(Overlay::SpellCheck {
            segments, bad_words, current, suggestions: Vec::new(), done: true,
        }));
    }

    suggestions = app.spell
        .as_ref()
        .map(|c| c.suggest(&segments[bad_words[current]].0))
        .unwrap_or_default();

    Ok(Some(Overlay::SpellCheck {
        segments, bad_words, current, suggestions, done: false,
    }))
}

/// Reconstruct the corrected text from `segments` and write it back to the file.
fn save_spell_result(app: &mut App, segments: &[(String, bool)]) {
    let text: String = segments.iter().map(|(t, _)| t.as_str()).collect();
    match std::fs::write(&app.filepath, &text) {
        Ok(()) => app.content = text,
        Err(e) => crate::log::warn(&format!("save_spell_result: could not write {}: {e}", app.filepath.display())),
    }
}

// ─── Rendering ────────────────────────────────────────────────────────────────

fn render(frame: &mut Frame, app: &App) {
    let area  = frame.area();
    let theme = Theme::default();

    // ── Hint bar ─────────────────────────────────────────────────────────
    let hints: String = if app.searching {
        format!("  /{}_   Enter:confirm  ESC:cancel", app.search)
    } else if !app.search.is_empty() {
        format!(
            "  [/{}]  j/k:scroll  e:edit  S:spell  /:search  ?:help  Q:quit",
            app.search
        )
    } else {
        "  j/k:scroll  Ctrl-F/B:½page  e:edit  S:spell  /:search  ?:help  Q:quit".to_string()
    };
    let hint_h = hint_height_fn(&hints, area.width);

    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(hint_h)])
        .split(area);

    // ── Block border / title ──────────────────────────────────────────────
    let filename = app.filepath.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("?");
    let block_title = if app.searching {
        format!(" {}  /{} ", filename, app.search)
    } else if !app.search.is_empty() {
        format!(" {}  [/{}] ", filename, app.search)
    } else {
        format!(" {} ", filename)
    };
    let block = Block::default().borders(Borders::ALL).title(block_title);
    let content_area = block.inner(outer[0]);
    frame.render_widget(block, outer[0]);

    // ── Markdown rendering ────────────────────────────────────────────────
    let render_width = content_area.width as usize;
    let raw_lines    = ui::markdown::render_markdown(&app.content, render_width, &theme);

    // Apply search highlight.
    let lower_query: Vec<char> = app.search.to_lowercase().chars().collect();
    let md_lines = apply_search_highlight(raw_lines, &lower_query, search_hl_style());

    let total_lines    = md_lines.len();
    let content_height = content_area.height as usize;
    app.view_height.set(content_height);

    // Reserve one row for the scroll indicator when content overflows.
    let needs_scroll = total_lines > content_height;
    let view_height  = if needs_scroll { content_height.saturating_sub(1) } else { content_height };
    let max_scroll   = total_lines.saturating_sub(view_height);
    app.max_scroll.set(max_scroll);

    let scroll    = app.scroll.min(max_scroll);
    let has_above = scroll > 0;
    let has_below = scroll + view_height < total_lines;

    if needs_scroll {
        let end     = (scroll + view_height).min(total_lines);
        let visible: Vec<Line<'static>> = md_lines[scroll..end].to_vec();

        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(content_area);

        frame.render_widget(Paragraph::new(visible), sub[0]);

        let indicator = match (has_above, has_below) {
            (true,  true)  => format!("  ↑↓ {}/{}  j/k:scroll", scroll, max_scroll),
            (true,  false) => format!("  ↑ bottom  ({} lines total)  j/k:scroll", total_lines),
            (false, true)  => format!("  ↓ {} more lines  j/k:scroll", total_lines - end),
            (false, false) => String::new(),
        };
        frame.render_widget(
            Paragraph::new(indicator).style(Style::default().fg(Color::DarkGray)),
            sub[1],
        );
    } else {
        frame.render_widget(Paragraph::new(md_lines), content_area);
    }

    // ── Hint bar ──────────────────────────────────────────────────────────
    frame.render_widget(
        Paragraph::new(hints)
            .style(Style::default().fg(theme.hint_fg))
            .wrap(Wrap { trim: false }),
        outer[1],
    );

    // ── Overlay ───────────────────────────────────────────────────────────
    if app.overlay.is_some() {
        render_overlay(frame, area, app);
    }
}

// ─── Overlay rendering ────────────────────────────────────────────────────────

fn render_overlay(frame: &mut Frame, area: Rect, app: &App) {
    match &app.overlay {
        Some(Overlay::SpellCheck { segments, bad_words, current, suggestions, done }) => {
            render_spell_check(frame, area, segments, bad_words, *current, suggestions, *done);
        }
        Some(Overlay::Warn { title, body }) => {
            render_warn(frame, area, title, body);
        }
        Some(Overlay::KeyHelp { scroll }) => {
            render_key_help(frame, area, *scroll);
        }
        None => {}
    }
}

// ── Geometry helper ───────────────────────────────────────────────────────────

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

// ── Spell-check overlay ───────────────────────────────────────────────────────

fn render_spell_check(
    frame:       &mut Frame,
    area:        Rect,
    segments:    &[(String, bool)],
    bad_words:   &[usize],
    current:     usize,
    suggestions: &[String],
    done:        bool,
) {
    let theme   = Theme::default();
    let popup_w = area.width.min(76);
    let popup_h = 12u16;
    let popup   = centered_rect(popup_w, popup_h, area);
    frame.render_widget(Clear, popup);

    let title = if done { " Spell Check — Complete " } else { " Spell Check " };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.overlay_border_fg))
        .title(title);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if done {
        frame.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::raw(""),
                Line::from(Span::styled(
                    "  ✓ Spell check complete. No remaining errors.",
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )),
                Line::raw(""),
                Line::from(Span::styled(
                    "  Press any key to close.",
                    Style::default().fg(Color::DarkGray),
                )),
            ]),
            inner,
        );
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // text preview
            Constraint::Length(1), // current word label
            Constraint::Length(2), // suggestions
            Constraint::Length(1), // hint bar
        ])
        .split(inner);

    let current_seg_idx = bad_words.get(current).copied();
    let text_lines = build_text_window(segments, current_seg_idx, 6, chunks[0].width as usize);
    frame.render_widget(Paragraph::new(text_lines), chunks[0]);

    let current_word = current_seg_idx
        .and_then(|idx| segments.get(idx))
        .map(|(w, _)| w.as_str())
        .unwrap_or("");
    let remaining = bad_words.len().saturating_sub(current);
    let counter   = if remaining > 1 { format!("  ({} remaining)", remaining) } else { String::new() };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("\"{current_word}\""),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" not found{counter}"),
                Style::default().fg(Color::DarkGray),
            ),
        ])),
        chunks[1],
    );

    let sugg_lines = build_suggestion_lines(suggestions, chunks[2].width as usize);
    frame.render_widget(Paragraph::new(sugg_lines), chunks[2]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  1-9:replace  s:skip  a:accept-all  i:add-to-dict  ESC/q:done",
            Style::default().fg(theme.hint_fg),
        ))),
        chunks[3],
    );
}

/// Build a scrolling text window centred on the segment at `current_seg_idx`.
/// Mirrors the implementation in ptm's overlay.rs.
fn build_text_window(
    segments:        &[(String, bool)],
    current_seg_idx: Option<usize>,
    visible:         usize,
    width:           usize,
) -> Vec<Line<'static>> {
    let cur_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let width = if width == 0 { 70 } else { width };

    let mut all_lines: Vec<Line<'static>> = vec![Line::default()];
    let mut current_word_visual_line = 0usize;
    let mut x = 0usize;

    for (i, (text, _is_word)) in segments.iter().enumerate() {
        let style      = if Some(i) == current_seg_idx { cur_style } else { Style::default() };
        let is_current = Some(i) == current_seg_idx;
        let parts: Vec<&str> = text.split('\n').collect();

        for (j, part) in parts.iter().enumerate() {
            if j > 0 {
                all_lines.push(Line::default());
                x = 0;
            }
            let mut need_record = is_current && j == 0;
            if part.is_empty() {
                if need_record { current_word_visual_line = all_lines.len() - 1; }
                continue;
            }
            let mut remaining: &str = part;
            while !remaining.is_empty() {
                let available = width.saturating_sub(x);
                if available == 0 {
                    all_lines.push(Line::default());
                    x = 0;
                    continue;
                }
                if need_record {
                    current_word_visual_line = all_lines.len() - 1;
                    need_record = false;
                }
                let remaining_chars = remaining.chars().count();
                if remaining_chars <= available {
                    all_lines.last_mut().unwrap().spans.push(Span::styled(remaining.to_string(), style));
                    x += remaining_chars;
                    remaining = "";
                } else {
                    let avail_bytes = remaining.char_indices().nth(available).map(|(b, _)| b).unwrap_or(remaining.len());
                    let break_byte  = remaining[..avail_bytes].rfind(' ').map(|p| p + 1).unwrap_or(avail_bytes);
                    let (chunk, rest) = remaining.split_at(break_byte);
                    if !chunk.is_empty() {
                        all_lines.last_mut().unwrap().spans.push(Span::styled(chunk.to_string(), style));
                    }
                    all_lines.push(Line::default());
                    x = 0;
                    remaining = rest;
                }
            }
        }
    }

    let total = all_lines.len();
    let start = if total <= visible {
        0
    } else {
        let half = visible / 2;
        current_word_visual_line.saturating_sub(half).min(total - visible)
    };
    all_lines.into_iter().skip(start).take(visible).collect()
}

/// Format up to 9 numbered suggestions into at most 2 display lines.
fn build_suggestion_lines(suggestions: &[String], width: usize) -> Vec<Line<'static>> {
    if suggestions.is_empty() {
        return vec![
            Line::from(Span::styled("  (no suggestions)", Style::default().fg(Color::DarkGray))),
            Line::raw(""),
        ];
    }
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row = String::from("  ");
    for (i, s) in suggestions.iter().enumerate().take(9) {
        let entry  = format!("{}: {}", i + 1, s);
        let col_w  = (entry.len() + 2).max(14);
        let padded = format!("{:<col_w$}", entry);
        if row.len() + padded.len() > width && row.trim() != "" {
            lines.push(Line::from(Span::styled(row.clone(), Style::default().fg(Color::Cyan))));
            if lines.len() >= 2 { break; }
            row = format!("  {padded}");
        } else {
            row.push_str(&padded);
        }
    }
    if lines.len() < 2 && !row.trim().is_empty() {
        lines.push(Line::from(Span::styled(row, Style::default().fg(Color::Cyan))));
    }
    while lines.len() < 2 { lines.push(Line::raw("")); }
    lines
}

// ── Warn overlay ──────────────────────────────────────────────────────────────

fn render_warn(frame: &mut Frame, area: Rect, title: &str, body: &str) {
    let theme  = Theme::default();
    let lines: Vec<&str> = body.lines().collect();
    let height = (lines.len() as u16 + 4).max(6).min(area.height.saturating_sub(2));
    let width  = 62u16.min(area.width.saturating_sub(4));
    let popup  = centered_rect(width, height, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(title.to_string());
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let mut plines: Vec<Line> = vec![Line::raw("")];
    for l in &lines { plines.push(Line::from(Span::raw(format!("  {l}")))); }
    plines.push(Line::raw(""));
    plines.push(Line::from(Span::styled("  any key: close", Style::default().fg(theme.hint_fg))));

    frame.render_widget(Paragraph::new(plines).wrap(Wrap { trim: false }), inner);
}

// ── Key-help overlay ──────────────────────────────────────────────────────────

fn render_key_help(frame: &mut Frame, area: Rect, scroll: u16) {
    let theme  = Theme::default();
    let width  = 64u16.min(area.width.saturating_sub(4));
    let height = 22u16.min(area.height.saturating_sub(2));
    let popup  = centered_rect(width, height, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.overlay_border_fg))
        .title(" Key Bindings ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    let hdr  = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let key  = Style::default().fg(Color::Cyan);
    let desc = Style::default();

    macro_rules! row {
        ($k:literal, $d:literal) => {
            Line::from(vec![
                Span::styled(format!("  {:14}", $k), key),
                Span::styled($d, desc),
            ])
        };
    }

    let lines: Vec<Line<'static>> = vec![
        Line::from(Span::styled(" NAVIGATION", hdr)),
        row!("j / k",         "Scroll one line down / up"),
        row!("Ctrl-F / Ctrl-B","Scroll half-page down / up"),
        row!("PgDn / PgUp",   "Scroll half-page down / up"),
        row!("g / Home",      "Jump to top"),
        row!("G / End",       "Jump to bottom"),
        Line::raw(""),
        Line::from(Span::styled(" SEARCH", hdr)),
        row!("/",             "Begin incremental search"),
        row!("Enter",         "Confirm search, return to normal mode"),
        row!("ESC",           "Cancel / clear search"),
        Line::raw(""),
        Line::from(Span::styled(" EDITING", hdr)),
        row!("e",             "Open file in $EDITOR"),
        row!("S",             "Spell-check file, write corrections back"),
        Line::raw(""),
        Line::from(Span::styled(" SPELL CHECK OVERLAY", hdr)),
        row!("1-9",           "Replace with numbered suggestion"),
        row!("s",             "Skip this occurrence"),
        row!("a",             "Accept all occurrences (session)"),
        row!("i",             "Add to personal dictionary"),
        row!("ESC / q",       "Finish and save corrections"),
        Line::raw(""),
        Line::from(Span::styled(" GENERAL", hdr)),
        row!("?",             "Show this help"),
        row!("Q",             "Quit"),
    ];

    let total   = lines.len() as u16;
    let visible = chunks[0].height;
    let max_s   = total.saturating_sub(visible);
    let clamped = scroll.min(max_s);

    frame.render_widget(Paragraph::new(lines).scroll((clamped, 0)), chunks[0]);

    let hint = if total > visible {
        format!("  j/k:scroll ({}/{})   ESC:close", clamped + 1, max_s + 1)
    } else {
        "  ESC / ?:close".to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(hint, Style::default().fg(theme.hint_fg)))),
        chunks[1],
    );
}

// ─── Search-highlight helpers ─────────────────────────────────────────────────

fn search_hl_style() -> Style {
    Style::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

/// Split a `Span<'static>` into multiple spans, wrapping every case-insensitive
/// match of `lower_query` with `hl_style`.
fn split_span(span: Span<'static>, lower_query: &[char], hl_style: Style) -> Vec<Span<'static>> {
    let base = span.style;
    let text = span.content.into_owned();
    if text.is_empty() || lower_query.is_empty() {
        return vec![Span::styled(text, base)];
    }
    let text_chars:  Vec<char> = text.chars().collect();
    let lower_chars: Vec<char> = text.to_lowercase().chars().collect();
    let qlen = lower_query.len();
    let tlen = text_chars.len();

    let mut out: Vec<Span<'static>> = Vec::new();
    let mut i   = 0usize;
    let mut seg = 0usize;

    while i + qlen <= tlen {
        if lower_chars[i..i + qlen] == *lower_query {
            if i > seg {
                out.push(Span::styled(text_chars[seg..i].iter().collect::<String>(), base));
            }
            out.push(Span::styled(text_chars[i..i + qlen].iter().collect::<String>(), hl_style));
            seg = i + qlen;
            i   = seg;
        } else {
            i += 1;
        }
    }
    if seg < tlen {
        out.push(Span::styled(text_chars[seg..].iter().collect::<String>(), base));
    }
    if out.is_empty() {
        out.push(Span::styled(text_chars.iter().collect::<String>(), base));
    }
    out
}

/// Apply search highlights to every span in every rendered line.
fn apply_search_highlight(
    lines:       Vec<Line<'static>>,
    lower_query: &[char],
    hl_style:    Style,
) -> Vec<Line<'static>> {
    if lower_query.is_empty() {
        return lines;
    }
    lines
        .into_iter()
        .map(|line| {
            let spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .flat_map(|s| split_span(s, lower_query, hl_style))
                .collect();
            Line::from(spans)
        })
        .collect()
}

// ─── Hint-bar height helper ───────────────────────────────────────────────────

/// Returns the number of terminal rows the hint string occupies when ratatui
/// word-wraps it at `width` columns.  Capped at 3.
fn hint_height_fn(text: &str, width: u16) -> u16 {
    if width == 0 { return 1; }
    let len = text.chars().count();
    let w   = width as usize;
    (len.div_ceil(w).max(1) as u16).min(3)
}
