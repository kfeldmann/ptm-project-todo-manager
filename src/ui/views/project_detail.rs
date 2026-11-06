use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};

// ─── Search-highlight helpers ─────────────────────────────────────────────────

/// One search match, in coordinates of the *rendered* (wrapped) output.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchLoc {
    /// Line index into the rendered line vector.
    pub line: usize,
    /// Ordinal of this occurrence within its line (0-based), counted in
    /// match order across all spans of that line.
    pub occ:  usize,
}

/// Style applied to the *current* search match (the one pointed at by
/// `pd_match_idx`, i.e. the target of n/N navigation).
fn search_hl_style() -> Style {
    Style::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD)
}

/// Style applied to search matches that are *not* the current one.
fn search_hl_other_style() -> Style {
    Style::default()
        .bg(Color::DarkGray)
        .fg(Color::White)
}

/// Split a single `Span<'static>` into multiple spans, wrapping all
/// case-insensitive occurrences of `lower_query` (a char slice,
/// pre-lowercased) with a per-occurrence style.
///
/// `this_line` is the rendered line's index; `current` is the
/// (line, occ) location of the current match, if any.  Matching the current
/// location gets `current_style`; all other occurrences get `other_style`.
/// `next_occ` counts the occurrences seen so far in this line (callers pass
/// `Some(&mut counter)` so occurrences stay in sync across spans).
fn split_span(
    span: Span<'static>,
    lower_query: &[char],
    this_line: usize,
    current: Option<MatchLoc>,
    current_style: Style,
    other_style: Style,
    next_occ: Option<&mut usize>,
) -> Vec<Span<'static>> {
    let base = span.style;
    let text = span.content.into_owned();
    if text.is_empty() || lower_query.is_empty() {
        return vec![Span::styled(text, base)];
    }

    let fallback = &mut 0usize;
    let occ_counter: &mut usize = next_occ.unwrap_or(fallback);

    // The matching lives in `ui::text::split_matches`, which maps a
    // case-folded match back onto whole characters of the original text.
    // Using lowercased offsets directly against the original (as this used to)
    // mis-highlights whenever folding changes the length — `'İ'` lowercases to
    // two chars, so searching `İ` in `İstanbul` highlighted `İs` (finding #17).
    let mut out: Vec<Span<'static>> = Vec::new();
    for (seg, is_match) in crate::ui::text::split_matches(&text, lower_query) {
        if is_match {
            let is_current = current == Some(MatchLoc { line: this_line, occ: *occ_counter });
            let style = if is_current { current_style } else { other_style };
            out.push(Span::styled(seg, style));
            *occ_counter += 1;
        } else {
            out.push(Span::styled(seg, base));
        }
    }
    out
}

/// Re-process already-rendered `Vec<Line<'static>>` (e.g. from the markdown
/// renderer) and highlight all occurrences of `lower_query` within every span.
/// Returns the restyled lines plus the position of every match so callers can
/// navigate with n / N.  Every match gets the "other" style here; callers then
/// call `style_current_match` to paint the current match yellow.
fn apply_search_highlight(
    lines: Vec<Line<'static>>,
    lower_query: &[char],
) -> (Vec<Line<'static>>, Vec<MatchLoc>) {
    if lower_query.is_empty() {
        return (lines, Vec::new());
    }
    let mut positions: Vec<MatchLoc> = Vec::new();
    let lines = lines
        .into_iter()
        .enumerate()
        .map(|(li, line)| {
            let mut occ = 0usize;
            let spans: Vec<Span<'static>> = line
                .spans
                .into_iter()
                .flat_map(|span| {
                    let before = occ;
                    let spans = split_span(
                        span,
                        lower_query,
                        li,
                        None,
                        search_hl_other_style(),
                        search_hl_other_style(),
                        Some(&mut occ),
                    );
                    for k in before..occ {
                        positions.push(MatchLoc { line: li, occ: k });
                                        }
                    spans
                })
                .collect();
            Line::from(spans)
        })
        .collect();
    (lines, positions)
}

/// Restyle the current match: paint the match segment at `loc` (identified by
/// its dim-gray marker background) yellow, leaving all other matches gray.
/// Call this when the current-match pointer changes, before rendering.
fn style_current_match(
    lines: Vec<Line<'static>>,
    current: Option<MatchLoc>,
) -> Vec<Line<'static>> {
    if current.is_none() {
        return lines;
    }
    let cur = current.unwrap();
    let mut occ = 0usize;
    lines
        .into_iter()
        .enumerate()
        .map(|(li, line)| {
            occ = 0;
            Line::from(
                line.spans
                    .into_iter()
                    .map(|span| {
                        if span.style.bg == search_hl_other_style().bg {
                            let is_cur = cur.line == li;
                            let style = if is_cur && occ == cur.occ {
                                occ += 1;
                                search_hl_style()
                            } else {
                                if is_cur {
                                    occ += 1;
                                }
                                search_hl_other_style()
                            };
                            Span::styled(span.content.into_owned(), style)
                        } else {
                            span
                        }
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

use crate::app::{App, PdTab};
use crate::data::models::{TodoStatus, Update};

/// The current match location (from `pd_match_idx`, clamped), or `None` when
/// there are no matches.
fn current_match(app: &App, marks: &[MatchLoc]) -> Option<MatchLoc> {
    if marks.is_empty() {
        return None;
    }
    let clamped = app.pd_match_idx.min(marks.len() - 1);
    Some(marks[clamped])
}
use crate::ui::theme::Theme;
use super::reminder_date_style;

/// Register `marks` in [`crate::app::PublishedMetrics::match_marks`] so key
/// handlers can navigate with n / N.
///
/// This and the `.set(..)` calls below are the documented exception to
/// "rendering does not mutate state": the key handlers cannot compute wrapped
/// line positions or the viewport height themselves.  See
/// [`crate::app::PublishedMetrics`].
fn publish_match_marks(app: &App, marks: &[MatchLoc]) {
    *app.metrics.match_marks.borrow_mut() = marks.iter().map(|m| (m.line, m.occ)).collect();
}

// Left column: 11-char date/label; leading indent: 2 spaces; gap between cols: 2 spaces.
const INDENT: usize = 2;
const DATE_COL_WIDTH: usize = 11;
const COL_GAP: usize = 2;

/// The Project Detail hint bar for the current mode.
///
/// Kept as a function (rather than inline in `render`) so the strings can be
/// asserted against the key handlers: the hint bar drives the layout height,
/// so an inaccurate string is both a lie and a rendering bug.  Every key named
/// here must be one the handlers accept in that mode, and `n/N` must only be
/// advertised where match marks exist — the Description tab and the update
/// viewer.  On the Todos tab `n` adds a todo, so advertising match navigation
/// there is wrong.
fn hints_for(app: &App) -> String {
    if app.pd_adding_todo.is_some()
        || app.pd_editing_title.is_some()
        || app.pd_editing_todo.is_some()
    {
        "  Enter:save  ESC:cancel".into()
    } else if app.pd_searching {
        "  Enter:keep  ESC:clear".into()
    } else if app.pd_tab == PdTab::Updates && app.pd_open_update.is_some() {
        "  ESC:close  j/k:scroll  e:edit  S:spell  /:search  n/N:match  ?:help".into()
    } else if app.pd_tab == PdTab::Description {
        "  ESC:back  d/u/t/f:tabs  e/n:edit description  /:search  n/N:match  ?:help".into()
    } else {
        "  ESC:back  d/u/t/f:tabs  /:search  ?:help".into()
    }
}

pub fn render(frame: &mut Frame, area: Rect, app: &App) {
    let theme = Theme::default();

    let project = match &app.pd_project {
        Some(p) => p,
        None => return,
    };

    let hints = hints_for(app);

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
    let (plain_lines, marks) = apply_search_highlight(raw_lines, lower_query);
    let md_lines = style_current_match(plain_lines, current_match(app, &marks));
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
    app.metrics.desc_max_scroll.set(max_scroll as u16);
    app.metrics.desc_view_height.set(view_height as u16);

    // Register match positions for n/N navigation.
    publish_match_marks(app, &marks);

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
            (true,  true)  => format!("  ↑↓ {}/{} lines  j/k:^f/^b:g:G:scroll", scroll, max_scroll),
            (true,  false) => format!("  ↑ bottom reached  ({} lines)  j/k:^f/^b:g:G:scroll", total_lines),
            (false, true)  => format!("  ↓ {} more lines  j/k:^f/^b:g:G:scroll", total_lines - end),
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
        super::date_prefix(&upd.created_at).to_string()
    } else {
        upd.label.clone()
    };
    let label_lines = super::char_wrap(&display_label, DATE_COL_WIDTH);
    let body_lines  = super::word_wrap(&upd.body, body_width);
    let row_count   = label_lines.len().max(body_lines.len());

    let truncated    = row_count > max_content_rows;
    let visible_rows = if truncated { max_content_rows } else { row_count };

    let hl = search_hl_other_style();

    (0..visible_rows)
        .map(|i| {
            let label_part = label_lines.get(i).map(|s| s.as_str()).unwrap_or("");
            let body_part  = body_lines.get(i).map(|s| s.as_str()).unwrap_or("");
            // Pad by display columns, not characters: a label containing wide
            // text would otherwise push the body column out of alignment
            // (finding #16).
            let padded     = super::pad_to_width(label_part, DATE_COL_WIDTH);
            let date_style = Style::default().fg(Color::DarkGray);

            // Last visible row when truncated → end with an ellipsis, which is
            // the signal that more body text exists below.  Both the fit test
            // and the truncation are by column, so a wide character is never
            // split and the ellipsis always stays inside the budget.
            let body_owned: String;
            let effective_body: &str = if truncated && i + 1 == visible_rows {
                if body_part.is_empty() {
                    body_owned = "…".to_string();
                } else if crate::ui::text::display_width(body_part) + 1 > body_width {
                    // No room for the text plus the ellipsis: truncate to
                    // `body_width` columns, which reserves one for the `…`.
                    body_owned = crate::ui::text::ellipsize(body_part, body_width);
                } else {
                    body_owned = format!("{}…", body_part);
                }
                &body_owned
            } else {
                body_part
            };

            let mut spans: Vec<Span<'static>> = vec![Span::raw("  ")];
            spans.extend(split_span(
                Span::styled(padded, date_style),
                lower_query, usize::MAX, None,
                search_hl_style(), hl, None,
            ));
            spans.push(Span::raw("  "));
            spans.extend(split_span(
                Span::styled(effective_body.to_string(), Style::default()),
                lower_query, usize::MAX, None,
                search_hl_style(), hl, None,
            ));
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
            if upd.label.is_empty() { super::date_prefix(&upd.created_at) }
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
        super::date_prefix(&upd.created_at).to_string()
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

    // Apply search highlighting across the entire buffer, then publish the
    // marks and paint the current match yellow.
    let (moving_map, marks) = apply_search_highlight(all_lines, lower_query);
    let all_lines = style_current_match(moving_map, current_match(app, &marks));
    publish_match_marks(app, &marks);

    let total_lines = all_lines.len();
    let content_height = content_area.height as usize;
    let needs_scroll = total_lines > content_height;
    let view_height = if needs_scroll { content_height.saturating_sub(1) } else { content_height };
    let max_scroll = total_lines.saturating_sub(view_height);

    app.metrics.update_view_max_scroll.set(max_scroll as u16);
    app.metrics.update_view_height.set(view_height as u16);

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
            (true,  true)  => format!("  \u{2191}\u{2193} {}/{} lines  j/k:^f/^b:g:G:scroll", scroll, max_scroll),
            (true,  false) => format!("  \u{2191} bottom reached  ({} lines)  j/k:^f/^b:g:G:scroll", total_lines),
            (false, true)  => format!("  \u{2193} {} more lines  j/k:^f/^b:g:G:scroll", total_lines - end),
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

    let mut todo_items: Vec<Vec<Line>> = app.pd_todos.iter().enumerate().map(|(i, t)| {
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
            vec![Line::from(vec![
                Span::styled("  > ",           sel_hl),
                Span::styled(before,           sel_hl),
                Span::styled(cur_ch.to_string(), sel_hl.add_modifier(Modifier::REVERSED)),
                Span::styled(after,            sel_hl),
            ])]
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
            let reminder    = t.reminder.as_deref()
                .map(|r| super::date_prefix(r).to_string())
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
            let reminder_cols = if reminder.is_empty() {
                0
            } else {
                crate::ui::text::display_width(&reminder) + 2
            };
            let wrap_width = (content_area.width as usize)
                .saturating_sub(PREFIX_COLS + reminder_cols)
                .max(1);
            let title_lines = super::word_wrap(&t.title, wrap_width);
            let n = title_lines.len();

            let search_hl = search_hl_other_style();
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
                            split_span(
                                Span::styled(tl.clone(), base_text_style),
                                lower_query, usize::MAX, None,
                                search_hl_style(), search_hl, None,
                            )
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
                            split_span(
                                Span::styled(tl.clone(), base_text_style),
                                lower_query, usize::MAX, None,
                                search_hl_style(), search_hl, None,
                            )
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

            lines
        }
    }).collect();

    // Inline add-todo input row at the bottom (always selected when shown).
    if let Some(ref state) = app.pd_adding_todo {
        let avail = (content_area.width as usize).saturating_sub(4).max(1); // 4 = "  + "
        let (before, cur_ch, after) = state.display(avail);
        todo_items.push(vec![Line::from(vec![
            Span::styled("  + ",           sel_hl),
            Span::styled(before,           sel_hl),
            Span::styled(cur_ch.to_string(), sel_hl.add_modifier(Modifier::REVERSED)),
            Span::styled(after,            sel_hl),
        ])]);
    }

    // Done count footer.
    if app.hide_done && app.pd_done_count > 0 {
        todo_items.push(vec![Line::from(vec![
            Span::styled(
                format!("  [{} done — C to show]", app.pd_done_count),
                Style::default().fg(Color::DarkGray),
            ),
        ])]);
    }

    // Archived count footer.
    if !app.pd_show_archived_todos && app.pd_archived_todo_count > 0 {
        todo_items.push(vec![Line::from(vec![
            Span::styled(
                format!("  [{} archived — A to show]", app.pd_archived_todo_count),
                Style::default().fg(Color::DarkGray),
            ),
        ])]);
    }

    // Pagination: slice the items to the window that fits, reserving rows
    // for the footer(s) so they are never pushed off the bottom, and
    // leaving no space under a dropped multi-row item.  Leftover rows are
    // used to peek at the next item's first line.
    let area_height = content_area.height as usize;
    let item_heights: Vec<usize> = todo_items.iter().map(|lines| lines.len()).collect();
    let footer_count = if app.hide_done && app.pd_done_count > 0 { 1 } else { 0 }
        + if !app.pd_show_archived_todos && app.pd_archived_todo_count > 0 { 1 } else { 0 };
    let core_count = todo_items.len() - footer_count;
    // The footers are not selectable: anchor the window on the core
    // selection (the inline add-todo row, when present, is core item
    // `pd_todos.len()` and is covered by this clamp).
    let selection_rel = selection.map(|s| s.min(core_count.saturating_sub(1)));
    let plan = super::list_window_reserved(&item_heights, area_height, selection_rel, core_count);
    let (start, end) = (plan.start, plan.end);

    // Render the kept footers as a fixed strip pinned to the bottom of the
    // area; the core list is windowed into the rows above.
    let footer_area_height = plan.footer_rows.min(area_height) as u16;
    let footer_y = content_area.y + (area_height - footer_area_height as usize) as u16;
    let footer_lines: Vec<Line> = todo_items[core_count..core_count + plan.kept_footers]
        .iter()
        .flatten()
        .cloned()
        .collect();
    frame.render_widget(
        Paragraph::new(footer_lines),
        Rect { y: footer_y, height: footer_area_height, ..content_area },
    );

    let list_items: Vec<ListItem> = todo_items[start..end]
        .iter()
        .map(|lines| ListItem::new(lines.clone()))
        .collect();

    let mut list_state = ListState::default();
    list_state.select(selection_rel.map(|s| s - start));

    let todo_list = List::new(list_items)
        .highlight_style(Style::default())
        .highlight_symbol("");

    let list_area = Rect { height: plan.core_rows.min(area_height) as u16, ..content_area };
    frame.render_stateful_widget(todo_list, list_area, &mut list_state);

    // Peek: fill leftover rows above the footer strip with the first lines
    // of the next core item (only when footers are pinned below; when they
    // were dropped, `end` already points at the real next item).
    let rows_for_core = area_height.saturating_sub(plan.footer_rows);
    let remaining_rows = rows_for_core.saturating_sub(plan.core_rows);
    if remaining_rows > 0 && end < core_count {
        let peek_area = Rect {
            y:      content_area.y + plan.core_rows as u16,
            height: remaining_rows as u16,
            ..content_area
        };
        let peek_lines: Vec<Line> = todo_items[end][..remaining_rows.min(todo_items[end].len())].to_vec();
        frame.render_widget(Paragraph::new(peek_lines), peek_area);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, PdTab};

    fn mk_app() -> App {
        let conn = crate::data::db::open_in_memory().unwrap();
        App::with_conn(conn, None).unwrap()
    }

    // ── finding #6: the hint bar must match the key handlers ────────────────

    #[test]
    fn the_updates_hint_does_not_advertise_match_navigation_on_other_tabs() {
        let mut app = mk_app();
        // No project needed: `hints_for` only reads the tab/mode fields.
        app.pd_tab = PdTab::Todos;
        let hints = hints_for(&app);
        assert!(
            !hints.contains("n/N"),
            "n/N must not be advertised on the Todos tab (n adds a todo): {hints:?}"
        );
    }

    #[test]
    fn the_viewer_hint_names_the_keys_the_viewer_accepts() {
        let mut app = mk_app();
        app.pd_tab = PdTab::Updates;
        app.pd_open_update = Some("update-id".into());
        let hints = hints_for(&app);
        // The hint bar for the viewer used to advertise n/N and ? while the
        // key handler accepted neither.
        for expected in ["ESC:close", "n/N:match", "?:help", "/:search", "j/k"] {
            assert!(hints.contains(expected), "{hints:?} is missing {expected:?}");
        }
        assert!(hints.contains("S:spell"), "S works in the viewer: {hints:?}");
        assert!(hints.contains("e:edit"), "e works in the viewer: {hints:?}");
    }

    #[test]
    fn inline_edit_modes_take_over_the_hint_bar() {
        let mut app = mk_app();
        app.pd_adding_todo = Some(crate::input::InputState::new());
        assert_eq!(hints_for(&app), "  Enter:save  ESC:cancel");
        app.pd_adding_todo = None;
        app.pd_searching = true;
        assert_eq!(hints_for(&app), "  Enter:keep  ESC:clear");
    }

    // ── the hint bar and the Key Help agree ─────────────────────────────────

    /// Every key the hint bar advertises for a mode must also be listed in the
    /// `?` Key Help, and vice versa for the keys that mode accepts.  This is
    /// the "hint bar and `key_help_lines()` match reality" exit criterion for
    /// session E, made checkable.
    #[test]
    fn hint_bar_keys_appear_in_the_key_help() {
        let key_help: String = crate::ui::views::overlay::key_help_text(60);
        let mut app = mk_app();
        app.pd_tab = PdTab::Updates;
        app.pd_open_update = Some("update-id".into());

        // Tokens like "ESC:close", "n/N:match", "/:search", "?:help".
        for token in hints_for(&app).split_whitespace() {
            let Some((keys, _)) = token.split_once(':') else { continue };
            for k in keys.split('/') {
                if k.is_empty() || k == "j" {
                    continue; // covered by the "j / k" row
                }
                assert!(
                    key_help.contains(k),
                    "the hint bar advertises {k:?} but Key Help never mentions it"
                );
            }
        }
    }

    // ── finding #17: case-fold drift in the search highlighter ──────────────

    /// The highlighted segments produced by the real `split_span`, in order.
    fn highlighted(spans: &[Span<'static>]) -> Vec<String> {
        let other = search_hl_other_style().bg;
        spans
            .iter()
            .filter(|s| s.style.bg == other)
            .map(|s| s.content.to_string())
            .collect()
    }

    fn split_plain(text: &'static str, query: &str) -> Vec<Span<'static>> {
        let q: Vec<char> = query.to_lowercase().chars().collect();
        split_span(
            Span::styled(text, Style::default()),
            &q,
            usize::MAX,
            None,
            search_hl_style(),
            search_hl_other_style(),
            None,
        )
    }

    #[test]
    fn the_highlighter_does_not_drift_when_case_folding_changes_length() {
        // 'İ' lowercases to two chars; the old code used the lowercased
        // offsets against the original and highlighted `İs`.
        assert_eq!(highlighted(&split_plain("İstanbul", "İ")), vec!["İ"]);
        assert_eq!(highlighted(&split_plain("İstanbul", "i")), vec!["İ"]);
        // A match starting after the fold: old code reported `tanb`.
        assert_eq!(highlighted(&split_plain("İstanbul", "stan")), vec!["stan"]);
    }

    #[test]
    fn the_highlighter_keeps_every_character_of_the_span() {
        for (text, query) in [
            ("İstanbul", "i"),
            ("İstanbul", "İ"),
            ("hello world", "o"),
            ("no match", "zzz"),
            ("日本語", "本"),
        ] {
            let spans = split_plain(text, query);
            let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
            assert_eq!(joined, text, "query {query:?}");
        }
    }

    #[test]
    fn the_highlighter_counts_occurrences_across_a_span() {
        let spans = split_plain("aaa", "a");
        assert_eq!(highlighted(&spans), vec!["a", "a", "a"]);
    }

    // ── finding #16: wide text must be measured in columns ──────────────────

    #[test]
    fn a_cjk_update_label_pads_to_the_column_budget() {
        // `update_content_lines` pads the label column so the body lines up.
        // With `format!("{:<width$}")` a two-character CJK label (4 columns)
        // was padded to 11 *characters* = 13 columns, pushing the body right.
        let upd = Update {
            id: "u".into(),
            project_id: "p".into(),
            label: "日本".into(),
            body: "body".into(),
            sort_order: 0.0,
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        };
        let lines = update_content_lines(&upd, 20, 4, &[]);
        let first = lines.first().expect("at least one line");
        let label_span = first
            .spans
            .iter()
            .find(|s| s.content.contains('日'))
            .expect("label span");
        assert_eq!(
            crate::ui::text::display_width(&label_span.content),
            DATE_COL_WIDTH,
            "the label column must be {DATE_COL_WIDTH} columns wide, not chars"
        );
    }

    #[test]
    fn a_truncated_update_body_ends_with_an_ellipsis_inside_the_column_budget() {
        // The ellipsis is the only signal that more text exists below, so it
        // must survive the truncation — and must not push the line past the
        // body column.
        let body = "word ".repeat(40);
        let upd = Update {
            id: "u".into(),
            project_id: "p".into(),
            label: String::new(),
            body: body.clone(),
            sort_order: 0.0,
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        };
        let width = 20usize;
        let lines = update_content_lines(&upd, width, 2, &[]);
        let last = lines.last().expect("at least one line");
        // spans: ["  ", padded label, "  ", body…]
        let body_text: String = last
            .spans
            .iter()
            .skip(3)
            .map(|s| s.content.as_ref())
            .collect();
        assert!(
            body_text.trim_end().ends_with('…'),
            "a truncated body must end with an ellipsis: {body_text:?}"
        );
        assert!(
            crate::ui::text::display_width(body_text.trim_end()) <= width,
            "{body_text:?} exceeds the {width}-column body budget"
        );
    }

    #[test]
    fn a_cjk_truncated_body_never_splits_a_character() {
        // 20 wide characters = 40 columns; at 10 columns the ellipsis must
        // replace a whole character, not half of one.
        let upd = Update {
            id: "u".into(),
            project_id: "p".into(),
            label: String::new(),
            body: "日本語のテキストです。".into(),
            sort_order: 0.0,
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-01T00:00:00Z".into(),
        };
        let lines = update_content_lines(&upd, 10, 1, &[]);
        let last = lines.last().expect("one line");
        // spans: ["  ", padded label, "  ", body…]
        let body_text: String = last.spans.iter().skip(3).map(|s| s.content.as_ref()).collect();
        assert!(body_text.ends_with('…'), "{body_text:?}");
        assert!(
            crate::ui::text::display_width(&body_text) <= 10,
            "{body_text:?} exceeds the 10-column budget"
        );
    }
}
