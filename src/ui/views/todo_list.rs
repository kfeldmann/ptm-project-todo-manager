use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::App;
use crate::data::models::TodoStatus;
use crate::ui::theme::Theme;
use super::reminder_date_style;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = Theme::default();

    // Compute hints first so their height can drive the layout constraint.
    let hints = if app.tl_searching {
        "  Enter:confirm  ESC:clear"
    } else if app.tl_editing.is_some() {
        "  Enter:save  ESC:cancel"
    } else {
        "  Q:quit  ?:help"
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(super::hint_height(hints, area.width)),
        ])
        .split(area);

    let title = if app.tl_searching {
        format!(" All Todos  /{} ", app.tl_search)
    } else {
        let search_label = if app.tl_search.is_empty() {
            String::new()
        } else {
            format!(" [/{}]", app.tl_search)
        };
        let tag_label = if let Some(ref tag) = app.tag_filter {
            format!(" [{}]", tag)
        } else {
            String::new()
        };
        format!(" All Todos{}{} ", search_label, tag_label)
    };

    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(chunks[0]);
    frame.render_widget(block, chunks[0]);

    let hl = Style::default()
        .bg(theme.highlight_bg)
        .fg(theme.highlight_fg)
        .add_modifier(Modifier::BOLD);

    // Precompute which item index in the flat list the cursor maps to,
    // so we can apply highlight styles manually per span.
    let selected_item: Option<usize> = {
        let mut item_idx = 0usize;
        let mut flat = 0usize;
        let mut result = None;
        'outer: for group in &app.tl_groups {
            item_idx += 1; // group header
            for _ in &group.todos {
                if flat == app.tl_cursor {
                    result = Some(item_idx);
                    break 'outer;
                }
                item_idx += 1;
                flat += 1;
            }
        }
        result
    };

    // Build a flat list of items (group headers + todo rows).
    let mut items: Vec<ListItem> = Vec::new();
    let mut current_item_idx = 0usize;

    for group in &app.tl_groups {
        // Group header.
        let sep = "─".repeat(
            (inner.width as usize)
                .saturating_sub(group.project_title.len() + 5)
        );
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!(" ── {} {}", group.project_title, sep),
                Style::default().fg(theme.header_fg),
            ),
        ])));
        current_item_idx += 1;

        for todo in &group.todos {
            let is_selected = selected_item == Some(current_item_idx);

            let is_editing = app.tl_editing
                .as_ref()
                .map(|(id, _)| id == &todo.id)
                .unwrap_or(false);

            let sc       = todo.status.char_repr();
            // Dim archived todos when not selected.
            let sc_style = if todo.archived && !is_selected {
                Style::default().fg(Color::DarkGray)
            } else {
                status_style(&todo.status, &theme)
            };
            let base_text_style = if todo.archived && !is_selected {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            let reminder = todo.reminder.as_ref()
                .map(|r| r[..r.len().min(10)].to_string())
                .unwrap_or_default();

            let is_done_or_canceled = matches!(todo.status, TodoStatus::Done | TodoStatus::Canceled);
            let reminder_style = todo.reminder.as_deref()
                .map(|d| reminder_date_style(d, is_done_or_canceled))
                .unwrap_or_default();

            // Keep the colored badge (Red/Yellow/Green) visible even when selected;
            // gray/inactive reminders adopt the selection style instead.
            let eff_reminder_style = if is_selected && reminder_style.bg.is_none() {
                hl
            } else {
                reminder_style
            };

            let item = if is_editing {
                let state = app.tl_editing.as_ref().map(|(_, s)| s).unwrap();
                let reminder_cols = if reminder.is_empty() { 0 } else { reminder.chars().count() + 2 };
                let avail = (inner.width as usize)
                    .saturating_sub(4 + reminder_cols) // 4 = "  X "
                    .max(1);
                let (before, cur_ch, after) = state.display(avail);
                ListItem::new(Line::from(vec![
                    Span::styled(format!("  {} ", sc), sc_style.patch(hl)),
                    Span::styled(before,             hl),
                    Span::styled(cur_ch.to_string(), hl.add_modifier(Modifier::REVERSED)),
                    Span::styled(after,              hl),
                    Span::styled(if reminder.is_empty() { "" } else { "  " }, hl),
                    Span::styled(reminder,           eff_reminder_style),
                ]))
            } else {
                // Word-wrap the title to fit the available width, reserving space
                // for the "  X " prefix (4 cols) and the reminder suffix.
                const PREFIX_COLS: usize = 4;
                let reminder_cols = if reminder.is_empty() { 0 } else { reminder.chars().count() + 2 };
                let wrap_width = (inner.width as usize)
                    .saturating_sub(PREFIX_COLS + reminder_cols)
                    .max(1);
                let title_lines = super::word_wrap(&todo.title, wrap_width);
                let n = title_lines.len();

                let sc_span_style   = if is_selected { sc_style.patch(hl) } else { sc_style };
                let text_span_style = if is_selected { hl } else { base_text_style };
                let cont_span_style = if is_selected { hl } else { base_text_style };
                let spacer_style    = if is_selected { hl } else { base_text_style };

                let lines: Vec<Line> = title_lines
                    .iter()
                    .enumerate()
                    .map(|(i, tl)| {
                        let is_last = i == n - 1;
                        if i == 0 {
                            let mut spans = vec![
                                Span::styled(format!("  {} ", sc), sc_span_style),
                                Span::styled(tl.clone(), text_span_style),
                            ];
                            if n == 1 && !reminder.is_empty() {
                                spans.push(Span::styled("  ", spacer_style));
                                spans.push(Span::styled(reminder.clone(), eff_reminder_style));
                            }
                            Line::from(spans)
                        } else {
                            let mut spans = vec![Span::styled(format!("    {}", tl), cont_span_style)];
                            if is_last && !reminder.is_empty() {
                                spans.push(Span::styled("  ", spacer_style));
                                spans.push(Span::styled(reminder.clone(), eff_reminder_style));
                            }
                            Line::from(spans)
                        }
                    })
                    .collect();

                ListItem::new(Text::from(lines))
            };

            items.push(item);
            current_item_idx += 1;
        }
    }

    // Done count footer.
    if app.hide_done && app.tl_done_count > 0 {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!("  [{} done — C to show]", app.tl_done_count),
                Style::default().fg(Color::DarkGray),
            ),
        ])));
    }

    // Archived count footer (added before the list widget is built).
    if !app.tl_show_archived && app.tl_archived_count > 0 {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!("  [{} archived — A to show]", app.tl_archived_count),
                Style::default().fg(Color::DarkGray),
            ),
        ])));
    }

    let mut list_state = ListState::default();
    list_state.select(selected_item);

    let list = List::new(items)
        .highlight_style(Style::default())
        .highlight_symbol("");

    frame.render_stateful_widget(list, inner, &mut list_state);

    // Hint bar.
    frame.render_widget(
        Paragraph::new(hints)
            .style(Style::default().fg(theme.hint_fg))
            .wrap(Wrap { trim: false }),
        chunks[1],
    );
}

fn status_style(status: &TodoStatus, theme: &Theme) -> Style {
    match status {
        TodoStatus::New        => Style::default().fg(theme.new_fg),
        TodoStatus::InProgress => Style::default().fg(theme.in_progress_fg),
        TodoStatus::Done       => Style::default().fg(theme.done_fg),
        TodoStatus::Canceled   => Style::default().fg(theme.canceled_fg),
    }
}
