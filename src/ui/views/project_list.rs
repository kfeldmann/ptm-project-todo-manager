use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

use crate::app::App;
use crate::ui::theme::Theme;
use super::reminder_date_style;

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = Theme::default();

    // Compute hints first so their height can drive the layout constraint.
    let hints = if app.pl_creating.is_some() || app.pl_editing_title.is_some() {
        "  Enter:save  ESC:cancel"
    } else if app.pl_searching {
        "  Enter:confirm  ESC:clear"
    } else {
        "  Q:quit  ?:help"
    };

    // Outer split: main list + hint bar (height adapts to terminal width).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(super::hint_height(hints, area.width)),
        ])
        .split(area);

    // Block title shows search query or active tag filter.
    let title = if app.pl_searching {
        format!(" Projects  /{} ", app.pl_search)
    } else {
        let search_label = if app.pl_search.is_empty() {
            String::new()
        } else {
            format!(" [/{}]", app.pl_search)
        };
        let tag_label = if let Some(ref tag) = app.tag_filter {
            format!(" [{}]", tag)
        } else {
            String::new()
        };
        if search_label.is_empty() && tag_label.is_empty() {
            " Projects ".to_string()
        } else {
            format!(" Projects{}{} ", search_label, tag_label)
        }
    };

    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(chunks[0]);
    frame.render_widget(block, chunks[0]);

    // Determine cursor position first (needed for per-span highlight styling).
    let selection = if app.pl_creating.is_some() {
        Some(app.pl_projects.len())
    } else if !app.pl_projects.is_empty() {
        Some(app.pl_cursor.min(app.pl_projects.len().saturating_sub(1)))
    } else {
        None
    };

    // Pre-compute the inline-edit display for the selected row (if active).
    let editing_row_content: Option<(String, char, String)> = app.pl_editing_title.as_ref().map(|state| {
        let avail = (inner.width as usize).saturating_sub(2).max(1); // 2 for "> "
        let (before, cur_ch, after) = state.display(avail);
        (before, cur_ch, after)
    });

    let hl = Style::default()
        .bg(theme.highlight_bg)
        .fg(theme.highlight_fg)
        .add_modifier(Modifier::BOLD);

    // Build list items.
    let mut items: Vec<ListItem> = app.pl_projects.iter().enumerate().map(|(i, p)| {
        let is_selected = selection == Some(i);

        // Inline title-edit: replace the selected row with a cursor-aware input widget.
        if is_selected {
            if let Some((ref before, cur_ch, ref after)) = editing_row_content {
                return ListItem::new(Line::from(vec![
                    Span::styled("> ",             hl),
                    Span::styled(before.clone(),   hl),
                    Span::styled(cur_ch.to_string(), hl.add_modifier(Modifier::REVERSED)),
                    Span::styled(after.clone(),    hl),
                ]));
            }
        }


        let tags_str = if p.tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", p.tags.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(", "))
        };

        let reminder_str = p.soonest_reminder.as_ref()
            .map(|r| r[..r.len().min(10)].to_string())
            .unwrap_or_default();

        let title_base = if p.archived {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        };

        let reminder_style = p.soonest_reminder.as_deref()
            .map(|d| reminder_date_style(d, false))
            .unwrap_or_default();

        let title_style = if is_selected { title_base.patch(hl) } else { title_base };
        let tags_style  = if is_selected {
            Style::default().fg(theme.tag_fg).patch(hl)
        } else {
            Style::default().fg(theme.tag_fg)
        };

        let mut spans = vec![
            Span::styled(p.title.clone(), title_style),
            Span::styled(tags_str, tags_style),
        ];
        if !reminder_str.is_empty() {
            spans.push(Span::styled("  ", if is_selected { hl } else { Style::default() }));
            // Keep the colored reminder badge (Red/Yellow/Green) visible even when
            // selected.  Gray/inactive reminders adopt the selection style instead.
            let eff_reminder_style = if is_selected && reminder_style.bg.is_none() {
                hl
            } else {
                reminder_style
            };
            spans.push(Span::styled(reminder_str, eff_reminder_style));
        }
        ListItem::new(Line::from(spans))
    }).collect();

    // Inline creation row (shown at the bottom of the list).
    if let Some(ref state) = app.pl_creating {
        let prefix = "> ";
        let avail  = (inner.width as usize).saturating_sub(prefix.len()).max(1);
        let (before, cur_ch, after) = state.display(avail);
        items.push(ListItem::new(Line::from(vec![
            Span::styled(prefix,             hl),
            Span::styled(before,             hl),
            Span::styled(cur_ch.to_string(), hl.add_modifier(Modifier::REVERSED)),
            Span::styled(after,              hl),
        ])));
    }

    // Archived count footer.
    if !app.pl_show_archived && app.pl_archived_count > 0 {
        items.push(ListItem::new(Line::from(vec![
            Span::styled(
                format!("  [{} archived — A to show]", app.pl_archived_count),
                Style::default().fg(Color::DarkGray),
            ),
        ])));
    }

    let mut list_state = ListState::default();
    list_state.select(selection);

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
