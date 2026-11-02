use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::{App, Overlay};
use crate::input::InputState;
use crate::ui::theme::Theme;

pub fn render_overlay(frame: &mut Frame, area: Rect, app: &App) {
    match &app.overlay {
        Some(Overlay::QuickCapture { input }) => {
            render_quick_capture(frame, area, input);
        }
        Some(Overlay::TagPicker {
            all_tags, applied_ids, search, cursor, filter_mode, ..
        }) => {
            render_tag_picker(frame, area, all_tags, applied_ids, search, *cursor, *filter_mode);
        }
        Some(Overlay::ProjectPicker {
            projects, search, cursor, current_project_id, ..
        }) => {
            render_project_picker(frame, area, app, projects, search, *cursor, current_project_id);
        }
        Some(Overlay::ConfirmDelete { message, .. }) => {
            render_confirm_delete(frame, area, message);
        }
        Some(Overlay::ConfirmArchive { message, .. }) => {
            render_confirm_archive(frame, area, message);
        }
        Some(Overlay::ReminderInput { input, error, .. }) => {
            render_reminder_input(frame, area, input, error.as_deref());
        }
        Some(Overlay::RenameTag { input, error, .. }) => {
            render_rename_tag(frame, area, input, error.as_deref());
        }
        Some(Overlay::KeyHelp { scroll }) => {
            render_key_help(frame, area, *scroll, app);
        }
        Some(Overlay::SpellCheck { segments, bad_words, current, suggestions, done, .. }) => {
            render_spell_check(frame, area, segments, bad_words, *current, suggestions, *done);
        }
        Some(Overlay::Warn { title, body }) => {
            render_warn(frame, area, title, body);
        }
        None => {}
    }
}

// ─── Geometry helpers ─────────────────────────────────────────────────────────

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

// ─── Quick Capture ────────────────────────────────────────────────────────────

fn render_quick_capture(frame: &mut Frame, area: Rect, input: &InputState) {
    let theme = Theme::default();
    let popup = centered_rect(54, 5, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.overlay_border_fg))
        .title(" Quick Capture → Inbox ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // inner width (54 - 2 borders) minus "> " prefix = 50 usable columns
    let (before, cur_ch, after) = input.display(50);
    let text_style = Style::default().fg(Color::Reset);
    let content = Paragraph::new(vec![
        Line::from(vec![
            Span::raw("> "),
            Span::styled(before, text_style),
            Span::styled(cur_ch.to_string(), text_style.add_modifier(Modifier::REVERSED)),
            Span::styled(after, text_style),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "  ←→:cursor  Ctrl+A/E:home/end  Enter:save  ESC:cancel",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
    ]);
    frame.render_widget(content, inner);
}

// ─── Tag Picker ───────────────────────────────────────────────────────────────

fn render_tag_picker(
    frame: &mut Frame,
    area: Rect,
    all_tags: &[crate::data::models::Tag],
    applied_ids: &[String],
    search: &str,
    cursor: usize,
    filter_mode: bool,
) {
    let theme = Theme::default();

    let filtered: Vec<(usize, bool)> = all_tags
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            search.is_empty() || t.name.to_lowercase().contains(&search.to_lowercase())
        })
        .map(|(i, _)| (i, applied_ids.contains(&all_tags[i].id)))
        .collect();

    let exact_match = all_tags
        .iter()
        .any(|t| t.name.to_lowercase() == search.to_lowercase());
    let has_create = !search.is_empty() && !filter_mode && !exact_match;
    let item_count = filtered.len() + if has_create { 1 } else { 0 };

    let height = (item_count + 6).clamp(6, 23) as u16;
    let popup  = centered_rect(40, height, area);
    frame.render_widget(Clear, popup);

    let title = if filter_mode { " Tag Filter " } else { " Edit Tags " };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.overlay_border_fg))
        .title(title);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Split: search row + list + hint bar.
    let inner_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // search input
            Constraint::Min(1),    // tag list
            Constraint::Length(1), // hint bar
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("> "),
            Span::styled(format!("{}_", search), Style::default().fg(Color::Reset)),
        ])),
        inner_chunks[0],
    );

    let mut list_items: Vec<ListItem> = filtered.iter().map(|&(idx, is_applied)| {
        let tag    = &all_tags[idx];
        let prefix = if is_applied { "✓ " } else { "  " };
        let style  = if is_applied {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        ListItem::new(Line::from(vec![
            Span::styled(format!("{}{}", prefix, tag.name), style),
        ]))
    }).collect();

    if has_create {
        list_items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!("  + Create \"{}\"", search),
                Style::default().fg(Color::Yellow),
            ),
        ])));
    }

    let sel = if list_items.is_empty() {
        None
    } else {
        Some(cursor.min(list_items.len().saturating_sub(1)))
    };
    let mut list_state = ListState::default();
    list_state.select(sel);

    let list = List::new(list_items)
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, inner_chunks[1], &mut list_state);

    // Context-sensitive hint bar.
    let on_create_row = has_create && cursor >= filtered.len();
    let enter_hint = if on_create_row {
        "Enter:create"
    } else if filter_mode {
        "Enter:set-filter"
    } else {
        "Enter:toggle"
    };
    let esc_hint = if !search.is_empty() { "ESC:cancel" } else { "ESC:close" };
    let rename_hint = if !filter_mode && search.is_empty() && !filtered.is_empty() {
        "  r:rename"
    } else {
        ""
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!("  {}   {}{}", enter_hint, esc_hint, rename_hint),
                Style::default().fg(theme.hint_fg),
            ),
        ])),
        inner_chunks[2],
    );
}

// ─── Project Picker ───────────────────────────────────────────────────────────

fn render_project_picker(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    projects: &[crate::data::models::Project],
    search: &str,
    cursor: usize,
    current_project_id: &Option<String>,
) {
    let theme = Theme::default();
    let items = app.build_picker_items(projects, search, current_project_id);

    let height = (items.len() + 6).clamp(6, 23) as u16;
    let popup  = centered_rect(46, height, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.overlay_border_fg))
        .title(" Move to… ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let inner_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // search input
            Constraint::Min(1),    // project list
            Constraint::Length(1), // hint bar
        ])
        .split(inner);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw("> "),
            Span::styled(format!("{}_", search), Style::default().fg(Color::Reset)),
        ])),
        inner_chunks[0],
    );

    let list_items: Vec<ListItem> = items.iter().map(|(pid, name)| {
        let style = if pid.is_none() {
            Style::default().fg(Color::Blue) // inbox highlight
        } else {
            Style::default()
        };
        ListItem::new(Line::from(vec![Span::styled(format!("  {}", name), style)]))
    }).collect();

    let sel = if list_items.is_empty() {
        None
    } else {
        Some(cursor.min(list_items.len().saturating_sub(1)))
    };
    let mut list_state = ListState::default();
    list_state.select(sel);

    let list = List::new(list_items)
        .highlight_style(Style::default().bg(Color::DarkGray).fg(Color::White).add_modifier(Modifier::BOLD));
    frame.render_stateful_widget(list, inner_chunks[1], &mut list_state);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "  Enter:move   ESC:cancel",
                Style::default().fg(theme.hint_fg),
            ),
        ])),
        inner_chunks[2],
    );
}

// ─── Confirm Delete ───────────────────────────────────────────────────────────

fn render_confirm_delete(frame: &mut Frame, area: Rect, message: &str) {
    let theme = Theme::default();
    let popup = centered_rect(58, 6, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" Confirm Delete ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let content = Paragraph::new(vec![
        Line::raw(""),
        Line::from(vec![Span::raw(format!("  {}", message))]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "  y/Enter:confirm   n/ESC:cancel",
                Style::default().fg(theme.hint_fg),
            ),
        ]),
    ]);
    frame.render_widget(content, inner);
}

// ─── Confirm Archive ──────────────────────────────────────────────────────────

fn render_confirm_archive(frame: &mut Frame, area: Rect, message: &str) {
    let theme = Theme::default();
    let popup = centered_rect(58, 6, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Confirm Archive ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let content = Paragraph::new(vec![
        Line::raw(""),
        Line::from(vec![Span::raw(format!("  {}", message))]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(
                "  y/Enter:confirm   n/ESC:cancel",
                Style::default().fg(theme.hint_fg),
            ),
        ]),
    ]);
    frame.render_widget(content, inner);
}

// ─── Key Help ──────────────────────────────────────────────────────────────────

fn render_key_help(frame: &mut Frame, area: Rect, scroll: u16, app: &App) {
    let theme = Theme::default();
    let width  = 76u16.min(area.width.saturating_sub(4));
    let height = 30u16.min(area.height.saturating_sub(2));
    let popup  = centered_rect(width, height, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.overlay_border_fg))
        .title(" Key Bindings ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // Split inner: scrollable content + hint row.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // Key column is "  {:12}" = 14 columns; the rest is for descriptions.
    let desc_width = (chunks[0].width as usize).saturating_sub(KEY_COL_WIDTH);
    let lines = key_help_lines(desc_width);
    let content_height = lines.len() as u16;
    let visible = chunks[0].height;
    let max_scroll = content_height.saturating_sub(visible);
    app.key_help_max_scroll.set(max_scroll);
    let clamped = scroll.min(max_scroll);

    frame.render_widget(
        Paragraph::new(lines).scroll((clamped, 0)),
        chunks[0],
    );

    let scroll_hint = if content_height > visible {
        format!("  j/k:scroll ({}/{})   ESC:close", clamped + 1, max_scroll + 1)
    } else {
        "  ESC:close".to_string()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(scroll_hint, Style::default().fg(theme.hint_fg)),
        ])),
        chunks[1],
    );
}

/// Column the description starts at: the key column is `"  {:12}"` (14 cols)
/// and continuation lines use 14 spaces so the text stays aligned.
const KEY_COL_WIDTH: usize = 14;

/// Word-wraps a key binding row into one or more `Line`s.
///
/// `max_desc` is a **column** count, so every length here is counted in
/// characters, never bytes: slicing a `&str` at a byte offset that lands
/// inside a multi-byte character panics, and three of the key-help rows
/// contain `→` (finding #4).
///
/// `max_desc == 0` (a terminal narrower than the key column) disables
/// wrapping entirely.  It used to mean "hard-break at column 0", which looped
/// forever: `cut` became 0, `pos += 0 + 1` skipped one character, and the
/// remaining string could still be longer than 0, so the loop never ended.
/// Rendering the row unwrapped is what the old `d.len() <= max_desc` early
/// return intended for the ordinary case — let the paragraph clip it.
fn wrap_key_row(
    k: &'static str,
    d: &'static str,
    max_desc: usize,
    key_sty: Style,
    desc_sty: Style,
) -> Vec<Line<'static>> {
    let chars: Vec<char> = d.chars().collect();
    if max_desc == 0 || chars.len() <= max_desc {
        return vec![Line::from(vec![
            Span::styled(format!("  {:12}", k), key_sty),
            Span::styled(d, desc_sty),
        ])];
    }

    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut first = true;

    while pos < chars.len() {
        let prefix = if first {
            format!("  {:12}", k)
        } else {
            format!("  {:12}", "")
        };
        let remaining = chars.len() - pos;

        if remaining <= max_desc {
            let chunk: String = chars[pos..].iter().collect();
            out.push(Line::from(vec![
                Span::styled(prefix, key_sty),
                Span::styled(chunk, desc_sty),
            ]));
            break;
        }

        // Look one column past the budget for a space to break on; fall back
        // to a hard break when the budget holds no space (one long word).
        let window = &chars[pos..pos + max_desc + 1];
        let (cut, next) = match window.iter().rposition(|&c| c == ' ') {
            // Break on the space: it is dropped, not part of either line.
            Some(i) => (i, pos + i + 1),
            // Hard break: nothing is skipped, so no character is lost.
            None => (max_desc, pos + max_desc),
        };

        if cut == 0 {
            // The window starts with whitespace — skip it instead of emitting
            // a line that contains nothing.
            pos = next.max(pos + 1);
            continue;
        }

        let chunk: String = chars[pos..pos + cut].iter().collect();
        out.push(Line::from(vec![
            Span::styled(prefix, key_sty),
            Span::styled(chunk, desc_sty),
        ]));
        first = false;
        pos = next;
    }

    out
}

fn key_help_lines(desc_width: usize) -> Vec<Line<'static>> {
    let hdr  = Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let key  = Style::default().fg(Color::Cyan);
    let desc = Style::default();

    let mut out: Vec<Line<'static>> = Vec::new();

    macro_rules! section {
        ($title:expr) => { out.push(Line::from(vec![Span::styled($title, hdr)])) };
    }
    macro_rules! row {
        ($k:expr, $d:expr) => {
            out.extend(wrap_key_row($k, $d, desc_width, key, desc))
        };
    }

    section!(" GLOBAL");
    row!("i",       "Quick capture todo → Inbox");
    row!("?",       "Show this help");
    out.push(Line::raw(""));
    section!(" INLINE EDITING (where active)");
    row!("← / →",    "Move cursor");
    row!("Home / End", "Jump to start / end");
    row!("Delete",    "Delete character at cursor");
    row!("Ctrl+A/E",  "Home / End (readline style)");
    row!("Enter",     "Save");
    row!("ESC",       "Discard");
    out.push(Line::raw(""));
    out.push(Line::raw(""));

    section!(" PROJECT LIST");
    row!("j / k",   "Move cursor");
    row!("J / K",   "Reorder project");
    row!("Enter",   "Open project");
    row!("n",       "New project");
    row!("e",       "Edit project title inline");
    row!("/",       "Search");
    row!("a",       "Toggle archive (confirm)");
    row!("A",       "Show / hide archived");
    row!("D",       "Delete project");
    row!("T",       "Tag filter");
    row!("t",       "→ Todo List");
    row!("Q",       "Quit");
    out.push(Line::raw(""));
    section!(" PROJECT DETAIL");
    row!("j / k",   "Move cursor (Todos/Updates tabs) or scroll (Description/Update viewer)");
    row!("^f / ^b", "Half-page scroll (Description / Update viewer); PgDn / PgUp too");
    row!("g / G",   "Top / bottom (Description / Update viewer); Home / End too");
    row!("J / K",   "Reorder todo / update");
    row!("Enter",   "Open update in viewer (Updates tab)");
    row!("t/d/u/f", "Switch tabs");
    row!("T",       "Tag editor  (r:rename tag)");
    row!("e",       "Edit todo title inline (or open description $EDITOR in Description tab)");
    row!("s",       "Cycle todo status");
    row!("S",       "Spell check selected item (todo / update / description)");
    row!("r",       "Set / clear reminder");
    row!("m",       "Move todo to project");
    row!("a",       "Toggle archive (Todos tab, confirm)");
    row!("A",       "Show / hide archived todos (Todos tab)");
    row!("E",       "Edit project title");
    row!("C",       "Toggle hide done / canceled (Todos tab)");
    row!("D",       "Delete");
    row!("/",       "Search / highlight (all tabs + update viewer)");
    row!("n / N",   "Next / previous search match (wraps)  [Description tab & update viewer]");
    row!("ESC",     "Back to Project List (clears search if active)");
    out.push(Line::raw(""));
    section!(" TODO LIST");
    row!("j / k",   "Move cursor");
    row!("J / K",   "Reorder todo");
    row!("s",       "Cycle status");
    row!("S",       "Spell check selected todo");
    row!("e",       "Edit todo inline");
    row!("r",       "Set / clear reminder");
    row!("n",       "New todo");
    row!("m",       "Move to project");
    row!("/",       "Search");
    row!("a",       "Toggle archive (confirm)");
    row!("A",       "Show / hide archived");
    row!("C",       "Toggle hide done / canceled");
    row!("T",       "Tag filter");
    row!("D",       "Delete todo");
    row!("Enter",   "Open owning project");
    row!("p",       "→ Project List");
    row!("Q",       "Quit");
    out.push(Line::raw(""));
    section!(" SPELL CHECK OVERLAY");
    row!("1-9",     "Replace with numbered suggestion");
    row!("s",       "Skip this occurrence");
    row!("a",       "Accept all occurrences (ignore for session)");
    row!("i",       "Add to personal dictionary (persisted)");
    row!("ESC/q",   "Finish and save corrections");

    out
}

// ─── Reminder Input ───────────────────────────────────────────────────────────

fn render_rename_tag(frame: &mut Frame, area: Rect, input: &InputState, error: Option<&str>) {
    let theme = Theme::default();
    let popup = centered_rect(54, 6, area);
    frame.render_widget(Clear, popup);

    let border_style = if error.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(theme.overlay_border_fg)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" Rename Tag ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // inner width (54 - 2 borders) minus "> " prefix = 50 usable columns
    let (before, cur_ch, after) = input.display(50);
    let text_style = Style::default().fg(Color::Reset);
    let middle_line = if let Some(msg) = error {
        Line::from(Span::styled(
            format!("  \u{26a0} {}", msg),
            Style::default().fg(Color::Red),
        ))
    } else {
        Line::raw("")
    };
    let content = Paragraph::new(vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("> "),
            Span::styled(before, text_style),
            Span::styled(cur_ch.to_string(), text_style.add_modifier(Modifier::REVERSED)),
            Span::styled(after, text_style),
        ]),
        middle_line,
        Line::from(vec![
            Span::styled(
                "  \u{2190}\u{2192}:cursor  Enter:rename  ESC:back",
                Style::default().fg(theme.hint_fg),
            ),
        ]),
    ]);
    frame.render_widget(content, inner);
}

fn render_reminder_input(frame: &mut Frame, area: Rect, input: &InputState, error: Option<&str>) {
    let theme = Theme::default();
    let popup = centered_rect(56, 6, area);
    frame.render_widget(Clear, popup);

    let border_style = if error.is_some() {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(theme.overlay_border_fg)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(" Set Reminder (YYYY-MM-DD, blank to clear) ");
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // inner width (56 - 2 borders) minus "> " prefix = 52 usable columns
    let (before, cur_ch, after) = input.display(52);
    let text_style = Style::default().fg(Color::Reset);
    let middle_line = if let Some(msg) = error {
        Line::from(Span::styled(
            format!("  ⚠ {}", msg),
            Style::default().fg(Color::Red),
        ))
    } else {
        Line::raw("")
    };
    let content = Paragraph::new(vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("> "),
            Span::styled(before, text_style),
            Span::styled(cur_ch.to_string(), text_style.add_modifier(Modifier::REVERSED)),
            Span::styled(after, text_style),
        ]),
        middle_line,
        Line::from(vec![
            Span::styled(
                "  ←→:cursor  Enter:save  ESC:cancel  (blank = clear)",
                Style::default().fg(theme.hint_fg),
            ),
        ]),
    ]);
    frame.render_widget(content, inner);
}

// ─── Spell Check ─────────────────────────────────────────────────────────────

fn render_spell_check(
    frame: &mut Frame,
    area: Rect,
    segments: &[(String, bool)],
    bad_words: &[usize],
    current: usize,
    suggestions: &[String],
    done: bool,
) {
    let theme = Theme::default();

    // Popup: 76 wide, 12 tall (2 borders + 6 text + 1 word + 2 suggestions + 1 hint).
    let popup_w = area.width.min(76);
    let popup_h = 12u16;
    let popup = centered_rect(popup_w, popup_h, area);
    frame.render_widget(Clear, popup);

    let title = if done { " Spell Check — Complete " } else { " Spell Check " };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.overlay_border_fg))
        .title(title);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    // ── Done screen ───────────────────────────────────────────────────────
    if done {
        frame.render_widget(
            Paragraph::new(vec![
                Line::raw(""),
                Line::raw(""),
                Line::from(Span::styled(
                    "  \u{2713} Spell check complete. No remaining errors.",
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

    // ── Active check layout: [text:6] [word:1] [suggestions:2] [hint:1] ──
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(inner);

    // ── Text window ───────────────────────────────────────────────────────
    let current_seg_idx = bad_words.get(current).copied();
    let text_lines = build_text_window(segments, current_seg_idx, 6, chunks[0].width as usize);
    frame.render_widget(
        Paragraph::new(text_lines),
        chunks[0],
    );

    // ── Current word label ────────────────────────────────────────────────
    let current_word = current_seg_idx
        .and_then(|idx| segments.get(idx))
        .map(|(w, _)| w.as_str())
        .unwrap_or("");
    let remaining = bad_words.len().saturating_sub(current);
    let counter = if remaining > 1 {
        format!("  ({} remaining)", remaining)
    } else {
        String::new()
    };
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

    // ── Suggestions ───────────────────────────────────────────────────────
    let sugg_lines = build_suggestion_lines(suggestions, chunks[2].width as usize);
    frame.render_widget(Paragraph::new(sugg_lines), chunks[2]);

    // ── Hint bar ──────────────────────────────────────────────────────────
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  1-9:replace  s:skip  a:accept-all  i:add-to-dict  ESC/q:done",
            Style::default().fg(theme.hint_fg),
        ))),
        chunks[3],
    );
}

/// Build a window of `visible` visual rows centred on the row that contains
/// the first character of the segment at `current_seg_idx`.  Long logical
/// lines are word-wrapped at `width` columns so that each element of the
/// returned `Vec<Line>` occupies exactly one terminal row.  The current word
/// segment is rendered with a bold yellow-on-black highlight.
fn build_text_window(
    segments: &[(String, bool)],
    current_seg_idx: Option<usize>,
    visible: usize,
    width: usize,
) -> Vec<Line<'static>> {
    let cur_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let width = if width == 0 { 70 } else { width };
    let mut all_lines: Vec<Line<'static>> = vec![Line::default()];
    let mut current_word_visual_line: usize = 0;
    let mut x: usize = 0; // current column (terminal cells)

    for (i, (text, _is_word)) in segments.iter().enumerate() {
        let style = if Some(i) == current_seg_idx { cur_style } else { Style::default() };
        let is_current = Some(i) == current_seg_idx;

        // Split on explicit newlines; each '\n' forces a new visual row.
        let parts: Vec<&str> = text.split('\n').collect();
        for (j, part) in parts.iter().enumerate() {
            if j > 0 {
                all_lines.push(Line::default());
                x = 0;
            }

            // We record the visual row when the first character of the current
            // word is actually placed — after any wrap that may move it to a
            // new line — so we use a flag rather than recording up front.
            let mut need_record = is_current && j == 0;

            if part.is_empty() {
                if need_record {
                    current_word_visual_line = all_lines.len() - 1;
                }
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

                // Record visual row just before placing the first character.
                if need_record {
                    current_word_visual_line = all_lines.len() - 1;
                    need_record = false;
                }

                let remaining_chars = remaining.chars().count();
                if remaining_chars <= available {
                    // Entire remaining slice fits on the current row.
                    all_lines
                        .last_mut()
                        .unwrap()
                        .spans
                        .push(Span::styled(remaining.to_string(), style));
                    x += remaining_chars;
                    remaining = "";
                } else {
                    // Must wrap. Find the byte offset of the `available`-th char.
                    let avail_bytes = remaining
                        .char_indices()
                        .nth(available)
                        .map(|(b, _)| b)
                        .unwrap_or(remaining.len());
                    // Prefer to break at the last space within that slice.
                    let break_byte = remaining[..avail_bytes]
                        .rfind(' ')
                        .map(|p| p + 1) // keep the space on the current row
                        .unwrap_or(avail_bytes); // hard break if no space found
                    let (chunk, rest) = remaining.split_at(break_byte);
                    if !chunk.is_empty() {
                        all_lines
                            .last_mut()
                            .unwrap()
                            .spans
                            .push(Span::styled(chunk.to_string(), style));
                    }
                    all_lines.push(Line::default());
                    x = 0;
                    remaining = rest;
                }
            }
        }
    }

    // Select a window of `visible` visual rows centred on `current_word_visual_line`.
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
            Line::from(Span::styled(
                "  (no suggestions)",
                Style::default().fg(Color::DarkGray),
            )),
            Line::raw(""),
        ];
    }

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut row = String::from("  ");

    for (i, s) in suggestions.iter().enumerate().take(9) {
        let entry = format!("{}: {}", i + 1, s);
        // Each column is at least (entry.len + 2) wide, minimum 14 chars.
        let col_w = (entry.len() + 2).max(14);
        let padded = format!("{:<col_w$}", entry);

        if row.len() + padded.len() > width && row.trim() != "" {
            lines.push(Line::from(Span::styled(row.clone(), Style::default().fg(Color::Cyan))));
            if lines.len() >= 2 {
                break;
            }
            row = format!("  {padded}");
        } else {
            row.push_str(&padded);
        }
    }
    if lines.len() < 2 && !row.trim().is_empty() {
        lines.push(Line::from(Span::styled(row, Style::default().fg(Color::Cyan))));
    }
    while lines.len() < 2 {
        lines.push(Line::raw(""));
    }
    lines
}

// ─── Warn / Error notice ──────────────────────────────────────────────────────

fn render_warn(frame: &mut Frame, area: Rect, title: &str, body: &str) {
    let theme = Theme::default();
    let lines: Vec<&str> = body.lines().collect();
    // 2 border rows + 1 blank top + body lines + 1 blank + 1 hint
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

    let mut paragraph_lines: Vec<Line> = vec![Line::raw("")];
    for l in &lines {
        paragraph_lines.push(Line::from(Span::raw(format!("  {l}"))));
    }
    paragraph_lines.push(Line::raw(""));
    paragraph_lines.push(Line::from(Span::styled(
        "  any key: close",
        Style::default().fg(theme.hint_fg),
    )));

    let content = Paragraph::new(paragraph_lines).wrap(Wrap { trim: false });
    frame.render_widget(content, inner);
}

#[cfg(test)]
mod tests {
    use super::{key_help_lines, wrap_key_row, KEY_COL_WIDTH};
    use ratatui::style::Style;
    use ratatui::text::Line;

    fn plain(s: &'static str, w: usize) -> Vec<Line<'static>> {
        wrap_key_row("x", s, w, Style::default(), Style::default())
    }

    /// The text of a rendered line, with the key column and its padding removed.
    fn desc_of(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<Vec<_>>()
            .join("")
    }

    // ── finding #4: byte-slicing the description panicked ────────────────────

    /// The original defect: `max_desc` is a column budget, but the code sliced
    /// `&d[pos..pos + cut]` on it as a byte offset, so a break that landed
    /// inside a multi-byte character panicked.  The observed panicking widths
    /// for the real key-help strings were 1, 2, 5, 6, 19 and 20.
    #[test]
    fn no_desc_width_panics_on_the_real_key_help_rows() {
        for w in 1..=60usize {
            let lines = key_help_lines(w);
            assert!(!lines.is_empty(), "desc_width {w} produced no lines");
        }
    }

    #[test]
    fn wrap_key_row_never_panics_on_a_multibyte_description() {
        for w in 1..=60usize {
            let lines = plain("Quick capture todo → Inbox", w);
            assert!(!lines.is_empty(), "desc_width {w} produced no lines");
        }
    }

    /// `desc_width == 0` used to loop forever (cut = 0, pos advanced by 1, the
    /// remaining string still longer than 0).  It must terminate and keep the
    /// whole description.
    #[test]
    fn zero_desc_width_terminates_and_keeps_the_text() {
        let lines = plain("Quick capture todo → Inbox", 0);
        assert_eq!(lines.len(), 1);
        assert!(desc_of(&lines[0]).contains("Quick capture todo → Inbox"));
    }

    // ── wrapping behaviour ──────────────────────────────────────────────────

    #[test]
    fn a_short_description_is_one_line() {
        let lines = plain("Save", 20);
        assert_eq!(lines.len(), 1);
        assert_eq!(desc_of(&lines[0]), format!("  {:12}Save", "x"));
    }

    #[test]
    fn a_long_description_wraps_on_spaces() {
        let lines = plain("alpha beta gamma delta", 11);
        assert!(lines.len() > 1);
        // Every line fits the column budget (the hard-break case aside).
        for line in &lines {
            let text = desc_of(line);
            let desc = &text[KEY_COL_WIDTH..];
            assert!(desc.chars().count() <= 11, "{desc:?} exceeds the budget");
        }
        // No character is lost or duplicated across the wrap.
        let joined: String = lines
            .iter()
            .map(|l| desc_of(l)[KEY_COL_WIDTH..].to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(joined.split_whitespace().collect::<Vec<_>>(),
                   ["alpha", "beta", "gamma", "delta"]);
    }

    #[test]
    fn continuation_lines_keep_the_description_column() {
        let lines = plain("alpha beta gamma delta", 11);
        assert!(lines.len() > 1);
        // The first line starts with the key column, the rest with 14 spaces.
        assert!(desc_of(&lines[0]).starts_with(&format!("  {:12}", "x")));
        for line in &lines[1..] {
            assert!(desc_of(line).starts_with(&" ".repeat(KEY_COL_WIDTH)));
        }
    }

    #[test]
    fn a_word_longer_than_the_budget_is_hard_broken() {
        // No space to break on: the row is split at the budget.
        let lines = plain("abcdefghijklmnop", 5);
        let chunks: Vec<String> = lines.iter().map(|l| desc_of(l)[KEY_COL_WIDTH..].to_string()).collect();
        assert_eq!(chunks, vec!["abcde", "fghij", "klmno", "p"]);
    }

    /// Multi-byte characters must be counted as one column each, not as their
    /// byte length — otherwise the row wraps far too early (or panics).
    #[test]
    fn multibyte_characters_count_as_one_column() {
        // Five `→` fit a budget of five; byte-wise they would need 15.
        let lines = plain("→→→→→ x", 5);
        let first = desc_of(&lines[0]);
        assert_eq!(&first[KEY_COL_WIDTH..], "→→→→→");
    }
}
