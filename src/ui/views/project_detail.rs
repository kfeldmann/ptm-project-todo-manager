use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

// ─── Search-highlight helpers ─────────────────────────────────────────────────

/// Style applied to text that matches the current search query.
fn search_hl_style() -> Style {
    Style::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

/// Split a single `Span<'static>` into multiple spans, wrapping all
/// case-insensitive occurrences of `lower_query` (a char slice, pre-lowercased)
/// with `hl_style`.
fn split_span(span: Span<'static>, lower_query: &[char], hl_style: Style) -> Vec<Span<'static>> {
    let base = span.style;
    let text = span.content.into_owned();
    if text.is_empty() || lower_query.is_empty() {
        return vec![Span::styled(text, base)];
    }
    let text_chars: Vec<char> = text.chars().collect();
    let lower_text: Vec<char> = text.to_lowercase().chars().collect();
    let qlen = lower_query.len();
    let tlen = text_chars.len();

    let mut out: Vec<Span<'static>> = Vec::new();
    let mut i = 0usize;
    let mut seg = 0usize;
    while i + qlen <= tlen {
        if lower_text[i..i + qlen] == *lower_query {
            if i > seg {
                out.push(Span::styled(
                    text_chars[seg..i].iter().collect::<String>(),
                    base,
                ));
            }
            out.push(Span::styled(
                text_chars[i..i + qlen].iter().collect::<String>(),
                hl_style,
            ));
            seg = i + qlen;
            i = seg;
        } else {
            i += 1;
        }
    }
    if seg < tlen {
        out.push(Span::styled(
            text_chars[seg..].iter().collect::<String>(),
            base,
        ));
    }
    if out.is_empty() {
        out.push(Span::styled(text_chars.iter().collect::<String>(), base));
    }
    out
}

/// Build `Vec<Span<'static>>` for `text`, highlighting all matches of
/// `lower_query` (pre-lowercased char slice) with `hl_style`.
fn highlight_spans(
    text: &str,
    lower_query: &[char],
    base: Style,
    hl_style: Style,
) -> Vec<Span<'static>> {
    split_span(Span::styled(text.to_string(), base), lower_query, hl_style)
}

/// Re-process already-rendered `Vec<Line<'static>>` (e.g. from the markdown
/// renderer) and highlight all occurrences of `lower_query` within every span.
fn apply_search_highlight(
    lines: Vec<Line<'static>>,
    lower_query: &[char],
    hl_style: Style,
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
                .flat_map(|span| split_span(span, lower_query, hl_style))
                .collect();
            Line::from(spans)
        })
        .collect()
}

use crate::app::{App, PdTab};
use crate::data::models::{TodoStatus, Update};
use crate::ui::theme::Theme;
use super::reminder_date_style;

// Left column: 11-char date/label; leading indent: 2 spaces; gap between cols: 2 spaces.
const INDENT: usize = 2;
const DATE_COL_WIDTH: usize = 11;
const COL_GAP: usize = 2;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = Theme::default();

    let project = match &app.pd_project {
        Some(p) => p,
        None => return,
    };

    // Hint bar text depends on mode and active tab.
    let hints: String = if app.pd_adding_todo.is_some()
        || app.pd_editing_title.is_some()
        || app.pd_editing_todo.is_some()
    {
        "  Enter:save  ESC:cancel".into()
    } else if app.pd_searching {
        "  Enter:keep  ESC:clear".into()
    } else {
        match app.pd_tab {
            PdTab::Updates if app.pd_open_update.is_some() => {
                "  ESC:close  ?:help".into()
            }
            _ => {
                "  ESC:back  d/u/t/f:tabs  ?:help".into()
            }
        }
    };

    // Outer layout: content block + hint bar.
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(4),
            Constraint::Length(super::hint_height(&hints, area.width)),
        ])
        .split(area);

    // Block border — title shows project name, search query indicator, or edit mode.
    let search_suffix = if !app.pd_search.is_empty() && !app.pd_searching {
        format!("  [/{}]", app.pd_search)
    } else {
        String::new()
    };
    let block_title = if app.pd_searching {
        format!(" {}  /{} ", project.title, app.pd_search)
    } else if app.pd_editing_title.is_some() {
        " ‹editing title› ".to_string()
    } else if project.archived {
        format!(" [archived] {}{} ", project.title, search_suffix)
    } else {
        format!(" {}{} ", project.title, search_suffix)
    };
    let block = Block::default().borders(Borders::ALL).title(block_title);
    let inner = block.inner(outer[0]);
    frame.render_widget(block, outer[0]);

    // Inner layout: [tags row] [tab bar] [tab content].
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // 0: tags / title-edit input
            Constraint::Length(1), // 1: tab bar
            Constraint::Min(0),    // 2: tab content
        ])
        .split(inner);

    // ── Section 0: tags or title-edit input ──────────────────────────────
    if let Some(ref state) = app.pd_editing_title {
        let prefix = "  > ";
        let avail  = (sections[0].width as usize).saturating_sub(prefix.len()).max(1);
        let (before, cur_ch, after) = state.display(avail);
        let text_style   = Style::default().fg(theme.highlight_fg).bg(theme.highlight_bg).add_modifier(Modifier::BOLD);
        let cursor_style = Style::default().fg(theme.highlight_bg).bg(theme.highlight_fg).add_modifier(Modifier::BOLD);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(prefix,             Style::default().fg(theme.highlight_fg).bg(theme.highlight_bg)),
                Span::styled(before,             text_style),
                Span::styled(cur_ch.to_string(), cursor_style),
                Span::styled(after,              text_style),
            ]))
            .style(Style::default().bg(theme.highlight_bg)),
            sections[0],
        );
    } else {
        let tags_str = if project.tags.is_empty() {
            "(no tags)".to_string()
        } else {
            format!(
                "[{}]",
                project.tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", ")
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("  Tags: ", Style::default().fg(Color::DarkGray)),
                Span::styled(tags_str, Style::default().fg(theme.tag_fg)),
            ])),
            sections[0],
        );
    }

    // ── Section 1: Tab bar ──────────────────────────────────────────────
    let tab_active   = Style::default()
        .fg(theme.highlight_fg)
        .bg(theme.highlight_bg)
        .add_modifier(Modifier::BOLD);
    let tab_inactive = Style::default().fg(Color::DarkGray);

    let tab_bar = Line::from(vec![
        Span::raw("  "),
        if matches!(app.pd_tab, PdTab::Description) {
            Span::styled(" Description ", tab_active)
        } else {
            Span::styled(" Description ", tab_inactive)
        },
        Span::raw("  "),
        if matches!(app.pd_tab, PdTab::Updates) {
            Span::styled(" Updates ", tab_active)
        } else {
            Span::styled(" Updates ", tab_inactive)
        },
        Span::raw("  "),
        if matches!(app.pd_tab, PdTab::Todos) {
            Span::styled(" Todos ", tab_active)
        } else {
            Span::styled(" Todos ", tab_inactive)
        },
        Span::raw("  "),
        if matches!(app.pd_tab, PdTab::Files) {
            Span::styled(" Files ", tab_active)
        } else {
            Span::styled(" Files ", tab_inactive)
        },
    ]);
    frame.render_widget(Paragraph::new(tab_bar), sections[1]);

    // Pre-compute the search highlight state once so tab renderers can use it.
    let lower_query: Vec<char> = app.pd_search.to_lowercase().chars().collect();

    // ── Section 2: Tab content ────────────────────────────────────────────
    match app.pd_tab {
        PdTab::Description  => render_description_tab(frame, sections[2], app, &theme, &lower_query),
        PdTab::Updates      => render_updates_tab(frame, sections[2], app, &theme, &lower_query),
        PdTab::Todos        => render_todos_tab(frame, sections[2], app, &theme, &lower_query),
        PdTab::Files        => render_files_tab(frame, sections[2]),
    }

    // ── Hint bar ──────────────────────────────────────────────────────────
    frame.render_widget(
        Paragraph::new(hints)
            .style(Style::default().fg(theme.hint_fg))
            .wrap(Wrap { trim: false }),
        outer[1],
    );
}

// ─── Description tab ──────────────────────────────────────────────────────────

fn render_description_tab(frame: &mut Frame, area: Rect, app: &App, theme: &Theme, lower_query: &[char]) {
    let project = match &app.pd_project {
        Some(p) => p,
        None => return,
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let content_area = layout[1];

    if project.description.trim().is_empty() {
        frame.render_widget(
            Paragraph::new("  (no description — press e to edit)")
                .style(Style::default().fg(Color::DarkGray)),
            content_area,
        );
        return;
    }

    let render_width = content_area.width as usize;
    let raw_lines = crate::ui::markdown::render_markdown(&project.description, render_width, theme);
    let md_lines = apply_search_highlight(raw_lines, lower_query, search_hl_style());
    let total_lines = md_lines.len();
    let content_height = content_area.height as usize;

    // When content overflows we reserve one row for the scroll indicator, so
    // the usable view height shrinks by one.  max_scroll must be computed
    // against that reduced height; otherwise the last line stays hidden and
    // the key handler can keep incrementing past the real bottom.
    let needs_scroll = total_lines > content_height;
    let view_height = if needs_scroll { content_height.saturating_sub(1) } else { content_height };
    let max_scroll = total_lines.saturating_sub(view_height);

    // Publish max_scroll so the key handler can clamp immediately and avoid
    // the overshoot-freeze (needing many k-presses to recover after j at bottom).
    app.pd_desc_max_scroll.set(max_scroll as u16);

    let scroll = (app.pd_desc_scroll as usize).min(max_scroll);
    let has_above = scroll > 0;
    let has_below = scroll + view_height < total_lines;

    if needs_scroll {
        let end = (scroll + view_height).min(total_lines);
        let visible: Vec<Line<'static>> = md_lines[scroll..end].to_vec();

        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(content_area);

        frame.render_widget(Paragraph::new(visible), sub[0]);

        let indicator = match (has_above, has_below) {
            (true,  true)  => format!("  ↑↓ {}/{} lines  j/k:scroll", scroll, max_scroll),
            (true,  false) => format!("  ↑ bottom reached  ({} lines)  j/k:scroll", total_lines),
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
}

// ─── Updates tab ──────────────────────────────────────────────────────────────

/// Returns rendered content lines for one update entry, capped at
/// `max_content_rows`.  When the body would exceed that cap the last rendered
/// line ends with `…` (or *is* `…` when that line would otherwise be blank).
/// The trailing blank-separator row is **not** included; callers append it.
fn update_content_lines(
    upd: &Update,
    body_width: usize,
    max_content_rows: usize,
    lower_query: &[char],
) -> Vec<Line<'static>> {
    let display_label = if upd.label.is_empty() {
        upd.created_at.chars().take(10).collect::<String>()
    } else {
        upd.label.clone()
    };
    let label_lines = super::char_wrap(&display_label, DATE_COL_WIDTH);
    let body_lines  = super::word_wrap(&upd.body, body_width);
    let row_count   = label_lines.len().max(body_lines.len());

    let truncated    = row_count > max_content_rows;
    let visible_rows = if truncated { max_content_rows } else { row_count };

    let hl = search_hl_style();

    (0..visible_rows)
        .map(|i| {
            let label_part = label_lines.get(i).map(|s| s.as_str()).unwrap_or("");
            let body_part  = body_lines.get(i).map(|s| s.as_str()).unwrap_or("");
            let padded     = format!("{:<width$}", label_part, width = DATE_COL_WIDTH);
            let date_style = Style::default().fg(Color::DarkGray);

            // Last visible row when truncated → end with an ellipsis.
            let body_owned: String;
            let effective_body: &str = if truncated && i + 1 == visible_rows {
                body_owned = if body_part.is_empty() {
                    "…".to_string()
                } else {
                    let chars: Vec<char> = body_part.chars().collect();
                    if chars.len() >= body_width {
                        format!(
                            "{}…",
                            chars[..body_width.saturating_sub(1)]
                                .iter()
                                .collect::<String>()
                        )
                    } else {
                        format!("{}…", body_part)
                    }
                };
                &body_owned
            } else {
                body_part
            };

            let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
            spans.extend(highlight_spans(&padded, lower_query, date_style, hl));
            spans.push(Span::raw("  "));
            spans.extend(highlight_spans(effective_body, lower_query, Style::default(), hl));
            Line::from(spans)
        })
        .collect()
}

fn render_updates_tab(frame: &mut Frame, area: Rect, app: &App, theme: &Theme, lower_query: &[char]) {
    // If an update is open in viewer mode, show the markdown view instead of the list.
    if app.pd_open_update.is_some() {
        render_update_viewer(frame, area, app, theme, lower_query);
        return;
    }

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let content_area = layout[1];

    if app.pd_updates.is_empty() {
        frame.render_widget(
            Paragraph::new("  (no updates — press n to add)")
                .style(Style::default().fg(Color::DarkGray)),
            content_area,
        );
        return;
    }

    // Body column width: total width minus leading indent, date col, and gap.
    let body_width = (content_area.width as usize)
        .saturating_sub(INDENT + DATE_COL_WIDTH + COL_GAP)
        .max(10);

    const MAX_BODY_ROWS: usize = 4;
    let n = app.pd_updates.len();
    let area_height = content_area.height as usize;

    // Rendered height of each entry: content rows (capped) + 1 blank separator.
    let item_heights: Vec<usize> = app.pd_updates.iter().map(|upd| {
        let label_lines = super::char_wrap(
            if upd.label.is_empty() { &upd.created_at[..upd.created_at.len().min(10)] }
            else { &upd.label },
            DATE_COL_WIDTH,
        );
        let body_lines = super::word_wrap(&upd.body, body_width);
        label_lines.len().max(body_lines.len()).min(MAX_BODY_ROWS) + 1
    }).collect();

    // Derive the first visible item so the selected entry is always on screen.
    let selected = app.pd_update_cursor.min(n.saturating_sub(1));
    let offset = {
        let sel_h = item_heights[selected].min(area_height);
        let mut remaining = area_height.saturating_sub(sel_h);
        let mut off = selected;
        while off > 0 {
            let h = item_heights[off - 1];
            if h <= remaining {
                remaining -= h;
                off -= 1;
            } else {
                break;
            }
        }
        off
    };

    // Count how many items fit fully from the offset.
    let mut rows_used: usize = 0;
    let mut num_full: usize = 0;
    let mut next_idx = offset;
    while next_idx < n {
        let h = item_heights[next_idx];
        if rows_used + h <= area_height {
            rows_used += h;
            num_full += 1;
            next_idx += 1;
        } else {
            break;
        }
    }
    // Edge case: tiny terminal where even one item doesn't fit — force it in.
    if num_full == 0 {
        num_full = 1;
        rows_used = item_heights[offset].min(area_height);
        next_idx = (offset + 1).min(n);
    }

    let last_full      = offset + num_full - 1;
    let remaining_rows = area_height.saturating_sub(rows_used);
    let peek_idx: Option<usize> =
        if remaining_rows > 0 && next_idx < n { Some(next_idx) } else { None };

    // Build list items for the fully-visible range.
    let items: Vec<ListItem> = app.pd_updates[offset..=last_full]
        .iter()
        .map(|upd| {
            let mut lines = update_content_lines(upd, body_width, MAX_BODY_ROWS, lower_query);
            lines.push(Line::raw("")); // blank separator
            ListItem::new(lines)
        })
        .collect();

    let rel_sel = if app.pd_editing_title.is_some() {
        None
    } else {
        Some(selected - offset)
    };
    let mut state = ListState::default();
    state.select(rel_sel);

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(theme.highlight_bg)
                .fg(theme.highlight_fg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("");

    // Render the list into only the rows it actually uses.
    let list_area = Rect { height: rows_used.min(area_height) as u16, ..content_area };
    frame.render_stateful_widget(list, list_area, &mut state);

    // Peek: fill any remaining space with the opening lines of the next entry.
    if let Some(pi) = peek_idx {
        let peek_area = Rect {
            y:      content_area.y + rows_used as u16,
            height: remaining_rows as u16,
            ..content_area
        };
        let peek_lines = update_content_lines(
            &app.pd_updates[pi],
            body_width,
            remaining_rows,
            lower_query,
        );
        let visible: Vec<Line<'static>> = peek_lines.into_iter().take(remaining_rows).collect();
        frame.render_widget(Paragraph::new(visible), peek_area);
    }
}

// ─── Todos tab ────────────────────────────────────────────────────────────────

fn render_update_viewer(frame: &mut Frame, area: Rect, app: &App, theme: &Theme, lower_query: &[char]) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let content_area = layout[1];

    let open_id = match &app.pd_open_update {
        Some(id) => id,
        None => return,
    };
    let upd = match app.pd_updates.iter().find(|u| &u.id == open_id) {
        Some(u) => u,
        None => return,
    };

    let label: String = if upd.label.is_empty() {
        upd.created_at.chars().take(10).collect()
    } else {
        upd.label.clone()
    };

    let render_width = content_area.width as usize;

    // Header (bold label) + blank separator + markdown body.
    let header: Line<'static> = Line::from(Span::styled(
        label,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let blank: Line<'static> = Line::raw("");
    let body_lines = crate::ui::markdown::render_markdown(&upd.body, render_width, theme);

    let mut all_lines: Vec<Line<'static>> = vec![header, blank];
    all_lines.extend(body_lines);

    // Apply search highlighting across the entire buffer.
    let all_lines = apply_search_highlight(all_lines, lower_query, search_hl_style());

    let total_lines = all_lines.len();
    let content_height = content_area.height as usize;
    let needs_scroll = total_lines > content_height;
    let view_height = if needs_scroll { content_height.saturating_sub(1) } else { content_height };
    let max_scroll = total_lines.saturating_sub(view_height);

    app.pd_update_view_max_scroll.set(max_scroll as u16);

    let scroll = (app.pd_update_view_scroll as usize).min(max_scroll);
    let has_above = scroll > 0;
    let has_below = scroll + view_height < total_lines;

    if needs_scroll {
        let end = (scroll + view_height).min(total_lines);
        let visible: Vec<Line<'static>> = all_lines[scroll..end].to_vec();

        let sub = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(content_area);

        frame.render_widget(Paragraph::new(visible), sub[0]);

        let indicator = match (has_above, has_below) {
            (true,  true)  => format!("  \u{2191}\u{2193} {}/{} lines  j/k:scroll", scroll, max_scroll),
            (true,  false) => format!("  \u{2191} bottom reached  ({} lines)  j/k:scroll", total_lines),
            (false, true)  => format!("  \u{2193} {} more lines  j/k:scroll", total_lines - end),
            (false, false) => String::new(),
        };
        frame.render_widget(
            Paragraph::new(indicator).style(Style::default().fg(Color::DarkGray)),
            sub[1],
        );
    } else {
        frame.render_widget(Paragraph::new(all_lines), content_area);
    }
}

fn render_todos_tab(frame: &mut Frame, area: Rect, app: &App, theme: &Theme, lower_query: &[char]) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    let content_area = layout[1];

    // Compute selection first so item rendering can apply highlight per span.
    let selection = if app.pd_editing_title.is_some() {
        None
    } else if app.pd_adding_todo.is_some() {
        Some(app.pd_todos.len())
    } else if !app.pd_todos.is_empty() {
        Some(app.pd_todo_cursor.min(app.pd_todos.len().saturating_sub(1)))
    } else {
        None
    };

    let sel_hl = Style::default()
        .bg(theme.highlight_bg)
        .fg(theme.highlight_fg)
        .add_modifier(Modifier::BOLD);

    let mut todo_items: Vec<ListItem> = app.pd_todos.iter().enumerate().map(|(i, t)| {
        let is_selected = selection == Some(i);

        let is_editing = app
            .pd_editing_todo
            .as_ref()
            .map(|(id, _)| id == &t.id)
            .unwrap_or(false);

        if is_editing {
            let state = app.pd_editing_todo.as_ref().map(|(_, s)| s).unwrap();
            let avail = (content_area.width as usize).saturating_sub(4).max(1); // 4 = "  > "
            let (before, cur_ch, after) = state.display(avail);
            ListItem::new(Line::from(vec![
                Span::styled("  > ",           sel_hl),
                Span::styled(before,           sel_hl),
                Span::styled(cur_ch.to_string(), sel_hl.add_modifier(Modifier::REVERSED)),
                Span::styled(after,            sel_hl),
            ]))
        } else {
            let sc          = t.status.char_repr();
            // Dim archived todos when not selected so they are visually distinct.
            let sc_style    = if t.archived && !is_selected {
                Style::default().fg(Color::DarkGray)
            } else {
                status_style(&t.status, theme)
            };
            let base_text_style = if t.archived && !is_selected {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            let reminder    = t.reminder.as_ref()
                .map(|r| r[..r.len().min(10)].to_string())
                .unwrap_or_default();
            let is_done_or_canceled = matches!(t.status, TodoStatus::Done | TodoStatus::Canceled);
            let reminder_style = t.reminder.as_deref()
                .map(|d| reminder_date_style(d, is_done_or_canceled))
                .unwrap_or_default();

            // Keep the colored badge (Red/Yellow/Green) visible even when selected;
            // gray/inactive reminders adopt the selection style instead.
            let eff_reminder_style = if is_selected && reminder_style.bg.is_none() {
                sel_hl
            } else {
                reminder_style
            };

            // Word-wrap the title to fit the available width, reserving space
            // for the "  X " prefix (4 cols) and the reminder suffix.
            const PREFIX_COLS: usize = 4;
            let reminder_cols = if reminder.is_empty() { 0 } else { reminder.chars().count() + 2 };
            let wrap_width = (content_area.width as usize)
                .saturating_sub(PREFIX_COLS + reminder_cols)
                .max(1);
            let title_lines = super::word_wrap(&t.title, wrap_width);
            let n = title_lines.len();

            let search_hl = search_hl_style();
            let lines: Vec<Line> = title_lines
                .iter()
                .enumerate()
                .map(|(i, tl)| {
                    let is_last = i == n - 1;
                    if i == 0 {
                        let sc_span = Span::styled(
                            format!("  {} ", sc),
                            if is_selected { sc_style.patch(sel_hl) } else { sc_style },
                        );
                        let text_spans: Vec<Span<'static>> = if is_selected {
                            vec![Span::styled(tl.clone(), sel_hl)]
                        } else {
                            highlight_spans(tl, lower_query, base_text_style, search_hl)
                        };
                        let mut spans = vec![sc_span];
                        spans.extend(text_spans);
                        if n == 1 && !reminder.is_empty() {
                            spans.push(Span::styled("  ", if is_selected { sel_hl } else { base_text_style }));
                            spans.push(Span::styled(reminder.clone(), eff_reminder_style));
                        }
                        Line::from(spans)
                    } else {
                        let prefix = if is_selected {
                            Span::styled("    ", sel_hl)
                        } else {
                            Span::styled("    ", base_text_style)
                        };
                        let text_spans: Vec<Span<'static>> = if is_selected {
                            vec![Span::styled(tl.clone(), sel_hl)]
                        } else {
                            highlight_spans(tl, lower_query, base_text_style, search_hl)
                        };
                        let mut spans = vec![prefix];
                        spans.extend(text_spans);
                        if is_last && !reminder.is_empty() {
                            spans.push(Span::styled("  ", if is_selected { sel_hl } else { base_text_style }));
                            spans.push(Span::styled(reminder.clone(), eff_reminder_style));
                        }
                        Line::from(spans)
                    }
                })
                .collect();

            ListItem::new(Text::from(lines))
        }
    }).collect();

    // Inline add-todo input row at the bottom (always selected when shown).
    if let Some(ref state) = app.pd_adding_todo {
        let avail = (content_area.width as usize).saturating_sub(4).max(1); // 4 = "  + "
        let (before, cur_ch, after) = state.display(avail);
        todo_items.push(ListItem::new(Line::from(vec![
            Span::styled("  + ",           sel_hl),
            Span::styled(before,           sel_hl),
            Span::styled(cur_ch.to_string(), sel_hl.add_modifier(Modifier::REVERSED)),
            Span::styled(after,            sel_hl),
        ])));
    }

    // Done count footer.
    if app.hide_done && app.pd_done_count > 0 {
        todo_items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!("  [{} done — C to show]", app.pd_done_count),
                Style::default().fg(Color::DarkGray),
            ),
        ])));
    }

    // Archived count footer.
    if !app.pd_show_archived_todos && app.pd_archived_todo_count > 0 {
        todo_items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!("  [{} archived — A to show]", app.pd_archived_todo_count),
                Style::default().fg(Color::DarkGray),
            ),
        ])));
    }

    let mut list_state = ListState::default();
    list_state.select(selection);

    let todo_list = List::new(todo_items)
        .highlight_style(Style::default())
        .highlight_symbol("");

    frame.render_stateful_widget(todo_list, content_area, &mut list_state);
}

// ─── Files tab─────────────────────────────────────────────────────────────────

fn render_files_tab(frame: &mut Frame, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    frame.render_widget(
        Paragraph::new("  (files — coming soon)")
            .style(Style::default().fg(Color::DarkGray)),
        layout[1],
    );
}

// ─── Shared helpers ───────────────────────────────────────────────────────────

fn status_style(status: &TodoStatus, theme: &Theme) -> Style {
    match status {
        TodoStatus::New        => Style::default().fg(theme.new_fg),
        TodoStatus::InProgress => Style::default().fg(theme.in_progress_fg),
        TodoStatus::Done       => Style::default().fg(theme.done_fg),
        TodoStatus::Canceled   => Style::default().fg(theme.canceled_fg),
    }
}
