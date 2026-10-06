//! Markdown → ratatui renderer.
//!
//! Entry point: [`render_markdown`] returns a `Vec<Line<'static>>` that can be
//! sliced for scrolling and passed directly to `Paragraph::new` — no `Wrap`
//! needed, because word-reflow is performed here.
//!
//! Supported Markdown elements
//! ───────────────────────────
//! • Headings H1–H6  (bold + per-level colour)
//! • Paragraphs; single newline → line break, double newline → paragraph gap
//! • **Bold**, *italic*, ~~strikethrough~~, `inline code`
//! • Fenced and indented code blocks (leading spaces preserved)
//! • > Blockquotes  (│ prefix, dark-gray text)
//! • Unordered lists  (• bullet, indented per nesting level)
//! • Ordered lists    (1. 2. …, indented per nesting level)
//! • Tables           (box-drawing borders, auto column widths; repeat the
//!                    delimiter row between data rows to draw a lighter
//!                    horizontal divider between those rows)
//! • Horizontal rules (─── separator line)

use pulldown_cmark::{Alignment, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::ui::theme::Theme;

/// Returns the terminal display width of a string, counting wide characters
/// (such as emoji and CJK) as 2 columns and narrow characters as 1.
#[inline]
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Display width of a single `char`.
#[inline]
fn char_display_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

// ─── Public entry point ───────────────────────────────────────────────────────

/// Render `md` as styled [`Line`]s word-wrapped to `width` columns.
///
/// The 2-space left indent used throughout the UI is included inside the
/// returned lines.  Pass `content_area.width as usize` unchanged — do **not**
/// subtract 2.
pub fn render_markdown(md: &str, width: usize, theme: &Theme) -> Vec<Line<'static>> {
    let mut ctx = Ctx::new(width, theme);
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    for event in Parser::new_ext(md, opts) {
        ctx.push(event);
    }
    if ctx.output.is_empty() {
        ctx.output.push(Line::from(""));
    }
    ctx.output
}

// ─── Inline token ─────────────────────────────────────────────────────────────

/// A single unit of inline content used during word reflow.
#[derive(Debug)]
enum Token {
    /// A non-whitespace run, together with its inline style.
    Word(String, Style),
    /// A line break (SoftBreak and HardBreak are both treated as hard breaks
    /// so that a single newline in the source becomes a visible line break).
    Break,
    /// Suppress the inter-word space that would normally precede the next
    /// `Word`.  Emitted when two text runs are adjacent across a markup
    /// boundary with no whitespace between them (e.g. `**bold**,`).
    Join,
}

// ─── List tracking ────────────────────────────────────────────────────────────

struct ListCtx {
    ordered:  bool,
    next_num: u64,
}

// ─── Renderer state ───────────────────────────────────────────────────────────

struct Ctx {
    // Completed output.
    output: Vec<Line<'static>>,

    // Inline token accumulator — drained by `flush`.
    tokens:    Vec<Token>,
    // Prefix for the first rendered line of the current block.
    first_pfx: String,
    // Prefix for continuation (wrapped) lines of the current block.
    cont_pfx:  String,
    // Base style applied to every word in the current block (e.g. heading colour).
    line_style: Style,
    // When true, emit a blank line before the next block.
    pending_blank: bool,

    // Active inline-style toggles (nested Start/End pairs).
    bold:          bool,
    italic:        bool,
    strikethrough: bool,

    // Block context.
    blockquote_depth:      usize,
    /// Set to `true` on `Start(BlockQuote)`, cleared on the first `Start(Paragraph)`
    /// inside that blockquote. Prevents a spurious blank bar line before the
    /// first paragraph of a freshly-opened blockquote.
    blockquote_just_opened: bool,
    list_stack:             Vec<ListCtx>,

    // List-item tracking.
    in_item:              bool,
    item_first_pfx:       String,
    item_cont_pfx:        String,
    item_paragraph_count: usize,

    // Code-block accumulation.
    in_code_block: bool,
    code_buffer:   String,

    // Tracks whether the most-recently-processed inline text event ended with
    // whitespace.  Used to decide whether to emit Token::Join between adjacent
    // styled runs (e.g. `**hello**,` → no space between "hello" and ",").
    last_text_had_trailing_space: bool,

    // Table accumulation.
    in_table:             bool,
    in_table_cell:        bool,
    table_alignments:     Vec<Alignment>,
    table_head_rows:      Vec<Vec<String>>,
    table_body_rows:      Vec<Vec<String>>,
    /// Parallel to `table_body_rows`; `true` means draw a mid-gray horizontal
    /// rule after that row (triggered by a repeated delimiter row in the source).
    table_body_sep_after: Vec<bool>,
    current_row:          Vec<String>,
    current_cell:         String,

    // Pre-computed styles.
    h1_style:             Style,
    h2_style:             Style,
    h3_style:             Style,
    h_lower_style:        Style,
    code_style:           Style,
    blockquote_style:     Style,
    rule_style:           Style,
    table_border_style:   Style,
    table_head_style:     Style,
    /// Lighter gray for optional horizontal lines between body rows.
    table_body_sep_style: Style,
    width: usize,
}

impl Ctx {
    fn new(width: usize, _theme: &Theme) -> Self {
        Ctx {
            output:        vec![],
            tokens:        vec![],
            first_pfx:     String::new(),
            cont_pfx:      String::new(),
            line_style:    Style::default(),
            pending_blank: false,
            bold:          false,
            italic:        false,
            strikethrough: false,
            blockquote_depth:      0,
            blockquote_just_opened: false,
            list_stack:             vec![],
            in_item:              false,
            item_first_pfx:       String::new(),
            item_cont_pfx:        String::new(),
            item_paragraph_count: 0,
            in_code_block: false,
            code_buffer:   String::new(),
            last_text_had_trailing_space: false,
            in_table:             false,
            in_table_cell:        false,
            table_alignments:     vec![],
            table_head_rows:      vec![],
            table_body_rows:      vec![],
            table_body_sep_after: vec![],
            current_row:          vec![],
            current_cell:         String::new(),
            // Heading colours: 20 navy → 26 cobalt → 62 slate → bold (no colour)
            h1_style:      Style::default().fg(Color::Indexed(20)).add_modifier(Modifier::BOLD),
            h2_style:      Style::default().fg(Color::Indexed(26)).add_modifier(Modifier::BOLD),
            h3_style:      Style::default().fg(Color::Indexed(62)).add_modifier(Modifier::BOLD),
            h_lower_style: Style::default().add_modifier(Modifier::BOLD),
            // Inline code: yellow
            code_style: Style::default().fg(Color::DarkGray),
            // Blockquote: dark-gray text
            blockquote_style: Style::default().fg(Color::DarkGray),
            // Horizontal rule & code-block gutter
            rule_style:         Style::default().fg(Color::DarkGray),
            table_border_style:   Style::default().fg(Color::DarkGray),
            table_head_style:     Style::default().add_modifier(Modifier::BOLD),
            // Body-row separator: mid-gray, visually lighter than the dark-gray border.
            table_body_sep_style: Style::default().fg(Color::Indexed(244)),
            // Reserve 1 column on the right so text never touches the border.
            width: width.saturating_sub(1).max(1),
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Current combined inline style (bold × italic × strikethrough).
    fn inline_style(&self) -> Style {
        let mut s = Style::default();
        if self.bold          { s = s.add_modifier(Modifier::BOLD); }
        if self.italic        { s = s.add_modifier(Modifier::ITALIC); }
        if self.strikethrough { s = s.add_modifier(Modifier::CROSSED_OUT); }
        s
    }

    /// Append a blank line, but never two blank lines in a row.
    fn push_blank(&mut self) {
        match self.output.last() {
            Some(l) if l.spans.is_empty() => {}
            _ => self.output.push(Line::from(vec![])),
        }
    }

    /// Like `push_blank`, but when inside a blockquote the gap line carries
    /// the `│` bar so the blockquote border spans paragraph breaks visually.
    fn push_blank_in_context(&mut self) {
        if self.blockquote_depth > 0 {
            let pfx   = self.bq_prefix();
            let style = self.blockquote_style;
            // Avoid double bar-only lines.
            let already = self.output.last().map_or(false, |l| {
                l.spans.len() == 1 && l.spans[0].content.as_ref() == pfx.as_str()
            });
            if !already {
                self.output.push(Line::from(vec![Span::styled(pfx, style)]));
            }
        } else {
            self.push_blank();
        }
    }

    /// Drain `tokens` into `output` as word-reflowed styled lines.
    fn flush(&mut self) {
        // A block boundary always resets the glue state — the first word of
        // the next block is never joined to the last word of this one.
        self.last_text_had_trailing_space = false;
        if self.tokens.is_empty() {
            self.first_pfx.clear();
            self.cont_pfx.clear();
            self.line_style = Style::default();
            return;
        }
        let tokens     = std::mem::take(&mut self.tokens);
        let first_pfx  = std::mem::take(&mut self.first_pfx);
        let cont_pfx   = std::mem::take(&mut self.cont_pfx);
        let line_style = std::mem::replace(&mut self.line_style, Style::default());

        self.output.extend(reflow(&tokens, self.width, &first_pfx, &cont_pfx, line_style));
    }

    /// Left-edge prefix for blockquote content at the current depth.
    fn bq_prefix(&self) -> String {
        let depth = self.blockquote_depth.max(1);
        // "  │ │ …" — 2-space base indent then one "│ " per nesting level.
        format!("{}{}", "  ", "│ ".repeat(depth))
    }

    // ── Table renderer ────────────────────────────────────────────────────────

    fn render_table(&mut self) {
        // Take ownership so we can push to self.output while iterating.
        let head_rows      = std::mem::take(&mut self.table_head_rows);
        let body_rows      = std::mem::take(&mut self.table_body_rows);
        let alignments     = std::mem::take(&mut self.table_alignments);
        let body_sep_after = std::mem::take(&mut self.table_body_sep_after);

        let indent = "  ";

        let col_count = head_rows.iter().chain(body_rows.iter())
            .map(|r| r.len())
            .max()
            .unwrap_or(0);
        if col_count == 0 { return; }

        // Per-column minimum width = longest whitespace-delimited token in any
        // cell.  A column scaled below this value will char-wrap at least one
        // word; we use it as a floor during proportional shrinking.
        let max_word_w: Vec<usize> = (0..col_count).map(|ci| {
            head_rows.iter().chain(body_rows.iter())
                .filter_map(|row| row.get(ci))
                .flat_map(|cell| cell.split_whitespace())
                .map(|w| display_width(w))
                .max()
                .unwrap_or(1)
                .max(1)
        }).collect();

        // Natural column widths (at least 1 char each).
        let mut col_w = vec![1usize; col_count];
        for row in head_rows.iter().chain(body_rows.iter()) {
            for (i, cell) in row.iter().enumerate() {
                if i < col_count {
                    col_w[i] = col_w[i].max(display_width(cell));
                }
            }
        }

        // Total width: indent + "│" + for each col: " " + content + " " + "│"
        let total = indent.len() + 1 + col_w.iter().map(|w| w + 3).sum::<usize>();
        if total > self.width {
            let avail = self.width.saturating_sub(indent.len() + 1 + col_count * 3);
            let sum_min: usize = max_word_w.iter().sum();

            if sum_min >= avail {
                // Even at per-word floors the table overflows; scale
                // proportionally from the floors, accepting char-wrapping.
                for (i, w) in col_w.iter_mut().enumerate() {
                    let mw = max_word_w[i];
                    *w = if sum_min == 0 { 1 }
                         else { ((mw as f64 * avail as f64 / sum_min as f64).floor() as usize).max(1) };
                }
            } else {
                // Give every column its per-word floor first, then distribute
                // the remaining budget proportionally to each column's natural
                // slack (natural_width − floor).  This prevents char-wrapping
                // while still filling as much of the available width as possible.
                let sum_slack: usize = col_w.iter().enumerate()
                    .map(|(i, &nw)| nw.saturating_sub(max_word_w[i]))
                    .sum();
                let remainder = avail - sum_min; // guaranteed ≥ 0
                for (i, w) in col_w.iter_mut().enumerate() {
                    let min_w = max_word_w[i];
                    let slack  = (*w).saturating_sub(min_w);
                    let extra  = if sum_slack == 0 { 0 }
                                 else { (slack as f64 * remainder as f64 / sum_slack as f64).floor() as usize };
                    *w = min_w + extra;
                }
            }
        }

        // Build a horizontal rule segment.
        let make_rule = |left: char, inner_sep: char, right: char| -> String {
            let mut s = format!("{}{}", indent, left);
            for (i, &w) in col_w.iter().enumerate() {
                s.push_str(&"─".repeat(w + 2));
                s.push(if i + 1 < col_count { inner_sep } else { right });
            }
            s
        };

        let top = make_rule('┌', '┬', '┐');
        let mid = make_rule('├', '┼', '┤');
        let bot = make_rule('└', '┴', '┘');

        let bdr_s      = self.table_border_style;
        let head_s     = self.table_head_style;
        let body_sep_s = self.table_body_sep_style;
        let head_count = head_rows.len();

        self.output.push(Line::from(Span::styled(top, bdr_s)));

        let all_rows: Vec<(Vec<String>, bool)> = head_rows.into_iter().map(|r| (r, true))
            .chain(body_rows.into_iter().map(|r| (r, false)))
            .collect();

        for (row_idx, (row, is_head)) in all_rows.iter().enumerate() {
            let cell_s = if *is_head { head_s } else { Style::default() };

            // Word-wrap each cell to its column width.
            let wrapped: Vec<Vec<String>> = col_w.iter().enumerate().map(|(ci, &w)| {
                let text = row.get(ci).map(String::as_str).unwrap_or("");
                word_wrap_cell(text, w)
            }).collect();

            let row_height = wrapped.iter().map(|v| v.len()).max().unwrap_or(1);

            for line_idx in 0..row_height {
                let mut spans: Vec<Span<'static>> = vec![
                    Span::raw(indent.to_string()),
                    Span::styled("│".to_string(), bdr_s),
                ];
                for (ci, &w) in col_w.iter().enumerate() {
                    let text = wrapped[ci].get(line_idx).map(String::as_str).unwrap_or("");
                    let tlen = display_width(text);
                    let pad = w.saturating_sub(tlen);
                    let cell_str = match alignments.get(ci).copied().unwrap_or(Alignment::None) {
                        Alignment::Right => format!(" {}{} ", " ".repeat(pad), text),
                        Alignment::Center => {
                            let lpad = pad / 2;
                            let rpad = pad - lpad;
                            format!(" {}{}{} ", " ".repeat(lpad), text, " ".repeat(rpad))
                        }
                        _ => format!(" {}{} ", text, " ".repeat(pad)),
                    };
                    spans.push(Span::styled(cell_str, cell_s));
                    spans.push(Span::styled("│".to_string(), bdr_s));
                }
                self.output.push(Line::from(spans));
            }

            // Separator lines between sections.
            let is_last_row = row_idx + 1 == all_rows.len();
            if *is_head && row_idx + 1 == head_count && !is_last_row {
                // Header/body divider — dark gray, same weight as the border.
                self.output.push(Line::from(Span::styled(mid.clone(), bdr_s)));
            } else if !*is_head && !is_last_row {
                // Optional body-row divider — mid-gray, lighter than the border.
                let body_idx = row_idx.saturating_sub(head_count);
                if body_sep_after.get(body_idx).copied().unwrap_or(false) {
                    self.output.push(Line::from(Span::styled(mid.clone(), body_sep_s)));
                }
            }
        }

        self.output.push(Line::from(Span::styled(bot, bdr_s)));
    }

    // ── Event handler ─────────────────────────────────────────────────────────

    #[allow(clippy::match_same_arms)]
    fn push(&mut self, event: Event<'_>) {
        // While inside a table cell, only text-producing events are collected;
        // structural and inline-style events are routed to the main match below.
        if self.in_table_cell {
            match &event {
                Event::Text(s)                     => { self.current_cell.push_str(s); return; }
                Event::Code(s)                     => { self.current_cell.push_str(s); return; }
                Event::SoftBreak | Event::HardBreak => { self.current_cell.push(' '); return; }
                // Suppress inline-style markers inside cells (plain text only).
                Event::Start(Tag::Strong)
                | Event::End(TagEnd::Strong)
                | Event::Start(Tag::Emphasis)
                | Event::End(TagEnd::Emphasis)
                | Event::Start(Tag::Strikethrough)
                | Event::End(TagEnd::Strikethrough) => { return; }
                _ => {} // structural events fall through
            }
        }

        match event {
            // ── Headings ─────────────────────────────────────────────────────
            Event::Start(Tag::Heading { level, .. }) => {
                if self.pending_blank { self.push_blank(); self.pending_blank = false; }
                self.flush();
                let (pfx, style) = match level {
                    HeadingLevel::H1 => ("  ", self.h1_style),
                    HeadingLevel::H2 => ("  ", self.h2_style),
                    HeadingLevel::H3 => ("  ", self.h3_style),
                    _                => ("    ", self.h_lower_style),
                };
                self.first_pfx  = pfx.to_string();
                self.cont_pfx   = pfx.to_string();
                self.line_style = style;
            }
            Event::End(TagEnd::Heading(_)) => {
                self.flush();
                self.pending_blank = true;
            }

            // ── Paragraphs ───────────────────────────────────────────────────
            Event::Start(Tag::Paragraph) => {
                if self.in_item {
                    // Inside a list item: reuse the item's computed prefix.
                    if self.item_paragraph_count == 0 {
                        self.first_pfx = self.item_first_pfx.clone();
                        self.cont_pfx  = self.item_cont_pfx.clone();
                    } else {
                        // Second+ paragraph in a loose list item: blank + continuation indent.
                        self.push_blank();
                        self.first_pfx = self.item_cont_pfx.clone();
                        self.cont_pfx  = self.item_cont_pfx.clone();
                    }
                    self.item_paragraph_count += 1;
                } else {
                    if self.pending_blank {
                        self.push_blank_in_context();
                        self.pending_blank = false;
                    } else if !self.output.is_empty() && !self.blockquote_just_opened {
                        self.push_blank_in_context();
                    }
                    self.blockquote_just_opened = false;
                    let pfx = if self.blockquote_depth > 0 {
                        self.bq_prefix()
                    } else {
                        "  ".to_string()
                    };
                    self.first_pfx = pfx.clone();
                    self.cont_pfx  = pfx;
                    if self.blockquote_depth > 0 {
                        self.line_style = self.blockquote_style;
                    }
                }
            }
            Event::End(TagEnd::Paragraph) => {
                self.flush();
                if !self.in_item {
                    self.pending_blank = true;
                }
            }

            // ── Blockquotes ──────────────────────────────────────────────────
            Event::Start(Tag::BlockQuote(_)) => {
                if self.pending_blank { self.push_blank(); self.pending_blank = false; }
                self.blockquote_depth += 1;
                self.blockquote_just_opened = true;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                self.pending_blank = true;
            }

            // ── Lists ────────────────────────────────────────────────────────
            Event::Start(Tag::List(start_num)) => {
                if self.pending_blank { self.push_blank(); self.pending_blank = false; }
                self.list_stack.push(ListCtx {
                    ordered:  start_num.is_some(),
                    next_num: start_num.unwrap_or(1),
                });
            }
            Event::End(TagEnd::List(_)) => {
                self.list_stack.pop();
                if self.list_stack.is_empty() {
                    self.pending_blank = true;
                }
            }
            Event::Start(Tag::Item) => {
                self.flush();
                // Nesting depth: 1 = top-level list.  Each extra level indents
                // 2 additional spaces so nested items visually step in.
                let depth      = self.list_stack.len(); // ≥ 1
                let base       = "  ";                  // 2-space left margin
                let extra      = "  ".repeat(depth.saturating_sub(1));
                let (first, cont) = if let Some(lc) = self.list_stack.last_mut() {
                    if lc.ordered {
                        let n      = lc.next_num;
                        lc.next_num += 1;
                        let bullet  = format!("{}. ", n);
                        let padding = " ".repeat(bullet.len()); // aligns wrapped text
                        (
                            format!("{}{}{}", base, extra, bullet),
                            format!("{}{}{}", base, extra, padding),
                        )
                    } else {
                        (
                            format!("{}{}• ", base, extra),
                            format!("{}{}  ", base, extra),
                        )
                    }
                } else {
                    ("  • ".to_string(), "    ".to_string())
                };
                self.item_first_pfx       = first.clone();
                self.item_cont_pfx        = cont.clone();
                self.first_pfx            = first;
                self.cont_pfx             = cont;
                self.in_item              = true;
                self.item_paragraph_count = 0;
            }
            Event::End(TagEnd::Item) => {
                // Tight list items put their text directly in the item (no
                // Start(Paragraph)); flush whatever was accumulated.
                self.flush();
                self.in_item = false;
            }

            // ── Code blocks ──────────────────────────────────────────────────
            Event::Start(Tag::CodeBlock(_)) => {
                if self.pending_blank { self.push_blank(); self.pending_blank = false; }
                else { self.push_blank(); }
                self.in_code_block = true;
                self.code_buffer.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                self.in_code_block = false;
                let code  = std::mem::take(&mut self.code_buffer);
                // Reserve space for "  ┃ " (4 chars) + at least 1 for content.
                let max_w = self.width.saturating_sub(5).max(1);
                let cs    = self.code_style;
                for raw_line in code.lines() {
                    for chunk in char_wrap_line(raw_line, max_w) {
                        self.output.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled("┃ ", Style::default().fg(Color::Yellow)),
                            Span::styled(chunk, cs),
                        ]));
                    }
                }
                // An empty code block produces no output; in either case close
                // with a blank so the next block has breathing room.
                self.push_blank();
                self.pending_blank = false;
            }

            // ── Tables ───────────────────────────────────────────────────────
            Event::Start(Tag::Table(alignments)) => {
                if self.pending_blank { self.push_blank(); self.pending_blank = false; }
                self.in_table      = true;
                self.in_table_cell = false;
                self.table_alignments = alignments.to_vec();
                self.table_head_rows.clear();
                self.table_body_rows.clear();
                self.table_body_sep_after.clear();
                self.current_row.clear();
                self.current_cell.clear();
            }
            Event::End(TagEnd::Table) => {
                self.in_table = false;
                self.render_table();
                self.pending_blank = true;
            }
            // In pulldown-cmark 0.13, TableHead contains cells *directly*
            // (no TableRow wrapper); body rows are wrapped in TableRow.
            Event::Start(Tag::TableHead) => {}
            Event::End(TagEnd::TableHead) => {
                // Flush the header cells that were accumulated directly.
                let row = std::mem::take(&mut self.current_row);
                if !row.is_empty() { self.table_head_rows.push(row); }
            }
            Event::Start(Tag::TableRow)   => { self.current_row.clear(); }
            Event::End(TagEnd::TableRow)  => {
                let row = std::mem::take(&mut self.current_row);
                if is_separator_row(&row) {
                    // The user repeated the delimiter row between data rows:
                    // mark the most-recently-added body row as needing a
                    // horizontal divider drawn after it.
                    if let Some(flag) = self.table_body_sep_after.last_mut() {
                        *flag = true;
                    }
                } else {
                    self.table_body_rows.push(row);
                    self.table_body_sep_after.push(false);
                }
            }
            Event::Start(Tag::TableCell)  => {
                self.current_cell.clear();
                self.in_table_cell = true;
            }
            Event::End(TagEnd::TableCell) => {
                self.in_table_cell = false;
                let cell = std::mem::take(&mut self.current_cell);
                self.current_row.push(cell);
            }

            // ── Horizontal rule ──────────────────────────────────────────────
            Event::Rule => {
                if self.pending_blank { self.push_blank(); self.pending_blank = false; }
                let rule_w = self.width.saturating_sub(4).max(1);
                self.output.push(Line::from(Span::styled(
                    format!("  {}", "─".repeat(rule_w)),
                    self.rule_style,
                )));
                self.pending_blank = true;
            }

            // ── Inline text ──────────────────────────────────────────────────
            Event::Text(s) => {
                if self.in_code_block {
                    self.code_buffer.push_str(&s);
                } else {
                    let style = self.inline_style();
                    let text  = s.as_ref();
                    let starts_with_space = text.starts_with(|c: char| c.is_whitespace());
                    let ends_with_space   = text.ends_with(|c: char| c.is_whitespace());
                    let mut any_words = false;
                    for (i, word) in text.split_whitespace().enumerate() {
                        // Insert a Join before the first word when it is
                        // adjacent (no whitespace) to the previous token across
                        // a markup boundary.
                        if i == 0 && !starts_with_space && !self.last_text_had_trailing_space {
                            self.tokens.push(Token::Join);
                        }
                        self.tokens.push(Token::Word(word.to_string(), style));
                        any_words = true;
                    }
                    self.last_text_had_trailing_space = if any_words {
                        ends_with_space
                    } else {
                        // Pure-whitespace text event — counts as trailing space.
                        !text.is_empty()
                    };
                }
            }
            // Inline code: keep backticks so it reads clearly in the terminal.
            Event::Code(s) => {
                // Emit Join if the code span is directly adjacent to the
                // preceding token with no whitespace between them.
                if !self.last_text_had_trailing_space {
                    self.tokens.push(Token::Join);
                }
                self.tokens.push(Token::Word(format!("`{}`", s), self.code_style));
                self.last_text_had_trailing_space = false;
            }
            // Both soft and hard breaks become a visible line break.
            Event::SoftBreak | Event::HardBreak => {
                if !self.in_code_block {
                    // Only insert a Break when there is already content on the
                    // line; a leading break would produce a spurious blank row.
                    if !self.tokens.is_empty() {
                        self.tokens.push(Token::Break);
                    }
                }
            }

            // ── Inline style toggles ─────────────────────────────────────────
            Event::Start(Tag::Strong)         => { self.bold          = true;  }
            Event::End(TagEnd::Strong)        => { self.bold          = false; }
            Event::Start(Tag::Emphasis)       => { self.italic        = true;  }
            Event::End(TagEnd::Emphasis)      => { self.italic        = false; }
            Event::Start(Tag::Strikethrough)  => { self.strikethrough = true;  }
            Event::End(TagEnd::Strikethrough) => { self.strikethrough = false; }

            // ── Links / images: render link text / alt text transparently ────
            Event::Start(Tag::Link { .. }) | Event::End(TagEnd::Link)   => {}
            Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image) => {}

            // ── Everything else: ignore ──────────────────────────────────────
            _ => {}
        }
    }
}

// ─── Word reflow ──────────────────────────────────────────────────────────────

/// Reflow a stream of styled inline tokens into `Vec<Line<'static>>`, honouring
/// `width`.  `first_pfx` is placed at the start of the first output line;
/// `cont_pfx` at the start of every subsequent (wrapped) line.  `base_style`
/// is patched onto every word's inline style so that, for example, heading
/// colour carries through to all words in the heading even when some are bold
/// or italic.
fn reflow(
    tokens:    &[Token],
    width:     usize,
    first_pfx: &str,
    cont_pfx:  &str,
    base_style: Style,
) -> Vec<Line<'static>> {
    if tokens.is_empty() {
        return vec![];
    }

    let fpw = display_width(first_pfx);
    let cpw = display_width(cont_pfx);

    let mut lines: Vec<Line<'static>> = vec![];
    let mut spans: Vec<Span<'static>> = vec![Span::styled(first_pfx.to_string(), base_style)];
    let mut cur_w         = fpw;
    let mut words_on_line = 0usize;
    // Style of the last word placed on the current line.  Used to style the
    // inter-word space so that runs of styled words (e.g. italic rendered as
    // reverse-video) are visually continuous, while the space *entering* a new
    // style is left unstyled (it belongs to the preceding, not the following,
    // word).
    let mut prev_style    = Style::default();

    let mut suppress_next_space = false;

    for token in tokens {
        match token {
            Token::Break => {
                // Only break if something is already on the line; skip leading
                // breaks so we don't emit a spurious empty row at the top.
                if words_on_line > 0 {
                    lines.push(Line::from(std::mem::take(&mut spans)));
                    spans.push(Span::styled(cont_pfx.to_string(), base_style));
                    cur_w         = cpw;
                    words_on_line = 0;
                    prev_style    = Style::default();
                }
                suppress_next_space = false;
            }
            Token::Join => {
                suppress_next_space = true;
            }
            Token::Word(word, style) => {
                let ww    = display_width(word);
                let no_space = suppress_next_space;
                suppress_next_space = false;
                let space = if words_on_line > 0 && !no_space { 1 } else { 0 };

                // Merge: heading/blockquote base colour + inline bold/italic.
                // base_style.patch(style): inline fg overrides base fg only when
                // explicitly set; modifiers are OR-combined.
                let final_style = base_style.patch(*style);

                // Wrap when the word doesn't fit — but always place at least
                // one word per line (avoids infinite loops on very long words).
                if words_on_line > 0 && cur_w + space + ww > width {
                    lines.push(Line::from(std::mem::take(&mut spans)));
                    spans.push(Span::styled(cont_pfx.to_string(), base_style));
                    cur_w         = cpw;
                    words_on_line = 0;
                } else if space > 0 {
                    // Style the space only when both neighbouring words share
                    // the same style — i.e. the space is inside a styled run.
                    // At a style boundary (prev ≠ next) the space is unstyled,
                    // so neither the entering nor the exiting style bleeds
                    // across the transition.
                    let space_style = if prev_style == final_style {
                        prev_style
                    } else {
                        Style::default()
                    };
                    spans.push(Span::styled(" ".to_string(), space_style));
                    cur_w += 1;
                }

                spans.push(Span::styled(word.clone(), final_style));
                cur_w         += ww;
                words_on_line += 1;
                prev_style     = final_style;
            }
        }
    }

    // Flush the last (possibly partial) line.
    if !spans.is_empty() {
        lines.push(Line::from(spans));
    }

    lines
}

// ─── Table helpers ───────────────────────────────────────────────────────────────────────

/// Returns `true` when every cell in `row` looks like a Markdown table
/// delimiter cell (e.g. `--`, `---`, `:--:`, `---:`).  Used to detect
/// when the user has repeated the delimiter row between data rows to
/// request an optional horizontal divider.
fn is_separator_row(row: &[String]) -> bool {
    !row.is_empty()
        && row.iter().all(|cell| {
            let c = cell.trim();
            !c.is_empty() && c.contains('-') && c.bytes().all(|b| b == b'-' || b == b':')
        })
}

// ─── Table cell word-wrapper ────────────────────────────────────────────────

/// Word-wrap `text` into lines of at most `width` visible characters,
/// breaking on whitespace boundaries.  Words longer than `width` are
/// hard-wrapped character by character.  Always returns at least one element.
fn word_wrap_cell(text: &str, width: usize) -> Vec<String> {
    if width == 0 || text.is_empty() {
        return vec![String::new()];
    }

    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut cur_w: usize = 0;

    for word in text.split_whitespace() {
        let wlen = display_width(word);

        if cur_w > 0 {
            if cur_w + 1 + wlen <= width {
                // Word fits on the current line (with a separating space).
                current.push(' ');
                current.push_str(word);
                cur_w += 1 + wlen;
                continue;
            }
            // Word doesn't fit: flush the current line and start a new one.
            lines.push(std::mem::take(&mut current));
            cur_w = 0;
        }

        // cur_w == 0: lay the word down, hard-wrapping if it exceeds `width`.
        if wlen <= width {
            current.push_str(word);
            cur_w = wlen;
        } else {
            // Hard-wrap character by character, accounting for wide chars.
            for ch in word.chars() {
                let cw = char_display_width(ch);
                if cur_w + cw > width && cur_w > 0 {
                    lines.push(std::mem::take(&mut current));
                    cur_w = 0;
                }
                current.push(ch);
                cur_w += cw;
            }
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }

    lines
}

// ─── Code-block line wrapper ─────────────────────────────────────────────────

/// Hard-wrap a single line (no embedded newlines) at `width` characters,
/// preserving every character including leading spaces.  Returns at least one
/// element.
fn char_wrap_line(s: &str, width: usize) -> Vec<String> {
    if display_width(s) <= width {
        return vec![s.to_string()];
    }
    let mut out: Vec<String> = vec![];
    let mut current = String::new();
    let mut cur_w = 0usize;
    for ch in s.chars() {
        let cw = char_display_width(ch);
        if cur_w + cw > width && cur_w > 0 {
            out.push(std::mem::take(&mut current));
            cur_w = 0;
        }
        current.push(ch);
        cur_w += cw;
    }
    if !current.is_empty() || out.is_empty() {
        out.push(current);
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{char_wrap_line, reflow, render_markdown, word_wrap_cell, Token};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::Line;
    use crate::ui::theme::Theme;

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn theme() -> Theme { Theme::default() }

    /// Concatenate all span contents in a line into a plain string.
    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Collect every `line_text` into one big string joined by `\n`.
    fn all_text(lines: &[Line<'static>]) -> String {
        lines.iter().map(|l| line_text(l)).collect::<Vec<_>>().join("\n")
    }

    // ── word_wrap_cell ─────────────────────────────────────────────────────────

    #[test]
    fn wwc_empty_returns_one_empty_string() {
        assert_eq!(word_wrap_cell("", 10), vec!["".to_string()]);
    }

    #[test]
    fn wwc_short_text_unchanged() {
        assert_eq!(word_wrap_cell("hello", 10), vec!["hello".to_string()]);
    }

    #[test]
    fn wwc_two_words_fit_on_one_line() {
        assert_eq!(word_wrap_cell("hi there", 12), vec!["hi there".to_string()]);
    }

    #[test]
    fn wwc_wraps_at_word_boundary() {
        // "hello" (5) + " world" (6) = 11 > 8 → wrap after "hello".
        assert_eq!(
            word_wrap_cell("hello world", 8),
            vec!["hello".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn wwc_three_words_across_two_lines() {
        // width=7: "foo bar" (7) fits; "baz" wraps to next line.
        assert_eq!(
            word_wrap_cell("foo bar baz", 7),
            vec!["foo bar".to_string(), "baz".to_string()]
        );
    }

    #[test]
    fn wwc_hard_wraps_long_word() {
        // "averylongword" (13 chars) with width 5 → ["avery", "longw", "ord"].
        assert_eq!(
            word_wrap_cell("averylongword", 5),
            vec!["avery".to_string(), "longw".to_string(), "ord".to_string()]
        );
    }

    // ── char_wrap_line ────────────────────────────────────────────────────────

    #[test]
    fn cwl_short_string_unchanged() {
        assert_eq!(char_wrap_line("hello", 80), vec!["hello"]);
    }

    #[test]
    fn cwl_exact_width_unchanged() {
        assert_eq!(char_wrap_line("hello", 5), vec!["hello"]);
    }

    #[test]
    fn cwl_splits_into_chunks() {
        assert_eq!(char_wrap_line("abcdefgh", 3), vec!["abc", "def", "gh"]);
    }

    #[test]
    fn cwl_preserves_leading_spaces() {
        // Leading spaces must be kept in the first chunk.
        let result = char_wrap_line("  hi", 80);
        assert_eq!(result, vec!["  hi"]);
    }

    #[test]
    fn cwl_empty_string() {
        // Empty input → one empty string ("at least one element" contract).
        assert_eq!(char_wrap_line("", 80), vec![""]);
    }

    // ── reflow ────────────────────────────────────────────────────────────────

    #[test]
    fn reflow_empty_tokens_returns_empty() {
        let lines = reflow(&[], 80, "  ", "  ", Style::default());
        assert!(lines.is_empty());
    }

    #[test]
    fn reflow_single_word() {
        let tokens = vec![Token::Word("hello".into(), Style::default())];
        let lines = reflow(&tokens, 80, "  ", "  ", Style::default());
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "  hello");
    }

    #[test]
    fn reflow_multiple_words_fit_one_line() {
        let tokens = vec![
            Token::Word("hi".into(),    Style::default()),
            Token::Word("there".into(), Style::default()),
        ];
        let lines = reflow(&tokens, 80, "  ", "  ", Style::default());
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "  hi there");
    }

    #[test]
    fn reflow_wraps_when_line_overflows() {
        // Width=10, prefix="  " (2 chars).
        // "hello" fits (2+5=7). "world": 7+1+5=13 > 10 → wrap.
        let tokens = vec![
            Token::Word("hello".into(), Style::default()),
            Token::Word("world".into(), Style::default()),
        ];
        let lines = reflow(&tokens, 10, "  ", "  ", Style::default());
        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "  hello");
        assert_eq!(line_text(&lines[1]), "  world");
    }

    #[test]
    fn reflow_break_token_forces_new_line() {
        let tokens = vec![
            Token::Word("line1".into(), Style::default()),
            Token::Break,
            Token::Word("line2".into(), Style::default()),
        ];
        let lines = reflow(&tokens, 80, "  ", "  ", Style::default());
        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "  line1");
        assert_eq!(line_text(&lines[1]), "  line2");
    }

    #[test]
    fn reflow_leading_break_skipped() {
        // A Break before any words must not emit a spurious blank line.
        let tokens = vec![
            Token::Break,
            Token::Word("hello".into(), Style::default()),
        ];
        let lines = reflow(&tokens, 80, "  ", "  ", Style::default());
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "  hello");
    }

    #[test]
    fn reflow_first_and_cont_prefix_differ() {
        // first line gets ">> "; continuation gets "   ".
        let tokens = vec![
            Token::Word("first".into(),  Style::default()),
            Token::Break,
            Token::Word("second".into(), Style::default()),
        ];
        let lines = reflow(&tokens, 80, ">> ", "   ", Style::default());
        assert_eq!(lines.len(), 2);
        assert!(line_text(&lines[0]).starts_with(">> "));
        assert!(line_text(&lines[1]).starts_with("   "));
    }

    #[test]
    fn reflow_base_style_applied_to_words() {
        // A cyan base style should propagate to rendered spans.
        let base = Style::default().fg(Color::Cyan);
        let tokens = vec![Token::Word("hi".into(), Style::default())];
        let lines = reflow(&tokens, 80, "  ", "  ", base);
        let word_span = lines[0].spans.iter().find(|s| s.content == "hi").unwrap();
        assert_eq!(word_span.style.fg, Some(Color::Cyan));
    }

    // ── render_markdown ───────────────────────────────────────────────────────

    #[test]
    fn rm_empty_input_returns_one_empty_line() {
        let lines = render_markdown("", 80, &theme());
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "");
    }

    #[test]
    fn rm_paragraph_indented_two_spaces() {
        let lines = render_markdown("Hello world", 80, &theme());
        let content = lines.iter().find(|l| line_text(l).contains("Hello")).unwrap();
        assert!(line_text(content).starts_with("  "),
            "expected 2-space indent, got {:?}", line_text(content));
    }

    #[test]
    fn rm_h1_navy_bold() {
        let lines = render_markdown("# Title", 80, &theme());
        let span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content.contains("Title")).unwrap();
        assert_eq!(span.style.fg, Some(Color::Indexed(20)));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn rm_h2_cobalt_bold() {
        let lines = render_markdown("## Subtitle", 80, &theme());
        let span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content.contains("Subtitle")).unwrap();
        assert_eq!(span.style.fg, Some(Color::Indexed(26)));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn rm_h3_slate_bold() {
        let lines = render_markdown("### Section", 80, &theme());
        let span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content.contains("Section")).unwrap();
        assert_eq!(span.style.fg, Some(Color::Indexed(62)));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn rm_h4_h5_h6_bold_no_colour() {
        for prefix in ["####", "#####", "######"] {
            let md = format!("{} Deep", prefix);
            let lines = render_markdown(&md, 80, &theme());
            let span = lines.iter().flat_map(|l| &l.spans)
                .find(|s| s.content.contains("Deep")).unwrap();
            assert!(span.style.add_modifier.contains(Modifier::BOLD),
                "expected BOLD for {}", prefix);
        }
    }

    #[test]
    fn rm_bold_inline() {
        let lines = render_markdown("Normal **bold** text", 80, &theme());
        let span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content == "bold").unwrap();
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn rm_italic_inline() {
        let lines = render_markdown("Normal *italic* text", 80, &theme());
        let span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content == "italic").unwrap();
        assert!(span.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn rm_italic_inter_word_space_is_styled() {
        // Spaces between italic words must carry ITALIC (styled with the
        // preceding word's style) so terminals that render italic as
        // reverse-video don't show jarring unstyled gaps.
        let lines = render_markdown("*hello world*", 80, &theme());
        // Find the space span between "hello" and "world".
        let space_span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content == " ").unwrap();
        assert!(
            space_span.style.add_modifier.contains(Modifier::ITALIC),
            "inter-word space should carry ITALIC, got style {:?}",
            space_span.style,
        );
    }

    #[test]
    fn rm_italic_boundary_spaces_are_unstyled() {
        // Spaces at style boundaries (entering or leaving italic) must NOT
        // carry ITALIC.  Only spaces *inside* a styled run should be styled.
        let lines = render_markdown("Normal *italic* text", 80, &theme());
        let spaces: Vec<_> = lines.iter().flat_map(|l| &l.spans)
            .filter(|s| s.content == " ")
            .collect();
        // There are two boundary spaces: before "italic" and after "italic".
        assert_eq!(spaces.len(), 2, "expected exactly 2 space spans");
        for sp in &spaces {
            assert!(
                !sp.style.add_modifier.contains(Modifier::ITALIC),
                "boundary space should NOT carry ITALIC, got style {:?}",
                sp.style,
            );
        }
    }

    #[test]
    fn rm_strikethrough_inline() {
        let lines = render_markdown("~~strike~~", 80, &theme());
        let span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content.contains("strike")).unwrap();
        assert!(span.style.add_modifier.contains(Modifier::CROSSED_OUT));
    }

    #[test]
    fn rm_inline_code_backtick_wrapped_dark_gray() {
        let lines = render_markdown("Use `foo()` here", 80, &theme());
        let span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content.contains("`foo()`")).unwrap();
        assert_eq!(span.style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn rm_fenced_code_block_gutter_marker() {
        let md = "```\nlet x = 1;\n```";
        let lines = render_markdown(md, 80, &theme());
        let code_line = lines.iter().find(|l| line_text(l).contains("┃"))
            .expect("expected a line with ┃ gutter");
        assert!(line_text(code_line).contains("let x = 1;"));
    }

    #[test]
    fn rm_unordered_list_bullet() {
        let md = "- item one\n- item two";
        let text = all_text(&render_markdown(md, 80, &theme()));
        assert!(text.contains("•"),       "expected bullet, got: {:?}", text);
        assert!(text.contains("item one"), "got: {:?}", text);
        assert!(text.contains("item two"), "got: {:?}", text);
    }

    #[test]
    fn rm_ordered_list_numbers() {
        let md = "1. first\n2. second";
        let text = all_text(&render_markdown(md, 80, &theme()));
        assert!(text.contains("1."),     "expected '1.', got: {:?}", text);
        assert!(text.contains("2."),     "expected '2.', got: {:?}", text);
        assert!(text.contains("first"),  "got: {:?}", text);
        assert!(text.contains("second"), "got: {:?}", text);
    }

    #[test]
    fn rm_nested_list_inner_indented_more() {
        let md = "- outer\n  - inner";
        let lines = render_markdown(md, 80, &theme());
        let outer = lines.iter().find(|l| line_text(l).contains("outer")).unwrap();
        let inner = lines.iter().find(|l| line_text(l).contains("inner")).unwrap();
        let outer_indent = line_text(outer).chars().take_while(|c| *c == ' ').count();
        let inner_indent = line_text(inner).chars().take_while(|c| *c == ' ').count();
        assert!(inner_indent > outer_indent,
            "inner_indent ({}) should be > outer_indent ({})", inner_indent, outer_indent);
    }

    #[test]
    fn rm_blockquote_bar_prefix_and_dark_gray() {
        let md = "> Quoted line";
        let lines = render_markdown(md, 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("│"),    "expected │ prefix, got: {:?}", text);
        assert!(text.contains("Quoted"), "got: {:?}", text);
        // The word span should carry the blockquote (DarkGray) style.
        let span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content.contains("Quoted")).unwrap();
        assert_eq!(span.style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn rm_blockquote_after_paragraph_no_leading_blank_bar() {
        // A blockquote preceded by a regular paragraph must NOT produce a
        // spurious bare-bar blank line at the very start of the quote block.
        let md = "Some text\n\n> A quote";
        let lines = render_markdown(md, 80, &theme());
        // The first line in the output that contains '│' must be the content
        // line itself, not a blank bar placeholder.
        let first_bar_line = lines.iter()
            .find(|l| line_text(l).contains('│'))
            .expect("expected at least one │ line");
        assert!(
            line_text(first_bar_line).contains("A quote"),
            "first │ line should be content, not a bare bar; got: {:?}",
            line_text(first_bar_line),
        );
    }

    #[test]
    fn rm_blockquote_multi_paragraph_stays_one_block() {
        // A blank line inside a blockquote (`> `) must NOT break the visual
        // bar — the gap line should still carry the │ prefix.
        let md = "> Paragraph 1\n> \n> Paragraph 2";
        let lines = render_markdown(md, 80, &theme());
        // Every line that is either content or a gap should carry the bar.
        for line in &lines {
            // Skip lines that are completely empty (before/after the block).
            if line.spans.is_empty() { continue; }
            let text = line.spans.iter().map(|s| s.content.as_ref()).collect::<String>();
            assert!(
                text.contains('│'),
                "line missing │ bar in multi-paragraph blockquote: {:?}",
                text
            );
        }
        // Both paragraphs must appear.
        let full = all_text(&lines);
        assert!(full.contains("Paragraph 1"), "got: {:?}", full);
        assert!(full.contains("Paragraph 2"), "got: {:?}", full);
    }

    #[test]
    fn rm_horizontal_rule_em_dashes() {
        let lines = render_markdown("---", 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("─"), "expected ─ characters, got: {:?}", text);
    }

    #[test]
    fn rm_table_borders_and_content() {
        let md = "| Col A | Col B |\n|-------|-------|\n| a     | b     |";
        let lines = render_markdown(md, 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("Col A"), "got: {:?}", text);
        assert!(text.contains("Col B"), "got: {:?}", text);
        assert!(text.contains("a"),     "got: {:?}", text);
        assert!(text.contains("b"),     "got: {:?}", text);
        assert!(text.contains("│"),     "no vertical border, got: {:?}", text);
        assert!(text.contains("─"),     "no horizontal border, got: {:?}", text);
    }

    #[test]
    fn rm_table_cell_wraps_long_content() {
        // At width=40 the table is narrow enough that a long cell must wrap.
        // All words should appear in the output and no ellipsis (…) should be present.
        let md = "| Name | Notes |\n|------|-------|\
\n| X    | This is a very long description that wraps |";
        let lines = render_markdown(md, 40, &theme());
        let text = all_text(&lines);
        assert!(text.contains("This"),  "missing 'This': {:?}",  text);
        assert!(text.contains("wraps"), "missing 'wraps': {:?}", text);
        assert!(!text.contains('…'),    "unexpected ellipsis: {:?}", text);
    }

    #[test]
    fn rm_table_narrow_column_not_char_wrapped() {
        // A column containing 1- and 2-digit numbers must not be char-wrapped
        // to width 1 when the table is squeezed.  "10" should never become
        // "1\n0".
        //
        // The table has two columns: a wide "Description" column and a narrow
        // "N" column with values 1..10.  At width=40 the proportional shrinker
        // would previously give the N column width 1.
        let md = "| Description | N |\n|-------------|---|\n\
| Some text   | 1 |\n\
| More text   | 10 |\n\
| Even more   | 25 |";  
        let lines = render_markdown(md, 40, &theme());
        let text = all_text(&lines);
        // "10" must appear whole on one line, not split across two.
        assert!(
            lines.iter().any(|l| line_text(l).contains("10")),
            "'10' was char-wrapped or missing: {}", text
        );
        assert!(
            lines.iter().any(|l| line_text(l).contains("25")),
            "'25' was char-wrapped or missing: {}", text
        );
        // Verify neither "1" followed by "0" on the very next content row
        // (a crude check that '10' wasn't split).
        let content: Vec<String> = lines.iter()
            .map(|l| line_text(l))
            .filter(|t| !t.trim().is_empty())
            .collect();
        for pair in content.windows(2) {
            assert!(
                !(pair[0].trim_end().ends_with('1') && pair[1].trim_start().starts_with('0')),
                "'10' appears to have been split across lines: {:?}", pair
            );
        }
    }

    #[test]
    fn rm_table_has_top_separator_and_bottom() {
        let md = "| A | B |\n|---|---|\n| 1 | 2 |";
        let text = all_text(&render_markdown(md, 80, &theme()));
        assert!(text.contains("┌"), "no top-left corner");
        assert!(text.contains("├"), "no header separator");
        assert!(text.contains("└"), "no bottom-left corner");
    }

    #[test]
    fn rm_word_wrap_produces_multiple_lines() {
        // Width=20, 2-char indent → ~18 usable chars. The string is 22 chars,
        // so it must wrap onto at least two content lines.
        let lines = render_markdown("Hello there world wide", 20, &theme());
        let non_blank: Vec<_> = lines.iter().filter(|l| !l.spans.is_empty()).collect();
        assert!(non_blank.len() >= 2,
            "expected wrapping but got {} non-blank lines", non_blank.len());
    }

    #[test]
    fn rm_soft_break_creates_new_line() {
        // A single \n inside a paragraph (SoftBreak) must become a visible line break.
        let lines = render_markdown("line one\nline two", 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("line one"), "got: {:?}", text);
        assert!(text.contains("line two"), "got: {:?}", text);
        // They must be on *separate* lines, not merged.
        let has_both = lines.iter().any(|l| {
            let t = line_text(l);
            t.contains("line one") && t.contains("line two")
        });
        assert!(!has_both, "'line one' and 'line two' must be on separate lines");
    }

    #[test]
    fn rm_no_consecutive_blank_lines() {
        // Multiple blank lines in the source must not produce back-to-back blank
        // lines in the output (push_blank deduplication).
        let lines = render_markdown("para one\n\n\n\npara two", 80, &theme());
        let mut prev_blank = false;
        for line in &lines {
            let blank = line.spans.is_empty();
            assert!(!prev_blank || !blank, "found two consecutive blank lines");
            prev_blank = blank;
        }
    }

    // ── Table body separators ─────────────────────────────────────────────────────

    #[test]
    fn rm_table_no_body_sep_without_repeated_delimiter() {
        // A plain table (no repeated delimiter row) must produce exactly one
        // ├ line: the header/body separator.
        let md = "| A | B |\n| -- | -- |\n| r1 | d1 |\n| r2 | d2 |";
        let lines = render_markdown(md, 80, &theme());
        let mid_lines: Vec<_> = lines.iter()
            .filter(|l| line_text(l).contains('├'))
            .collect();
        assert_eq!(mid_lines.len(), 1,
            "expected 1 separator line (header sep only), got {}", mid_lines.len());
    }

    #[test]
    fn rm_table_body_sep_from_repeated_delimiter() {
        // Repeating the delimiter row between data rows should produce an
        // additional ├ line (the body separator) in addition to the header separator.
        let md = "| A | B |\n\
                  | -- | -- |\n\
                  | r1 | d1 |\n\
                  | -- | -- |\n\
                  | r2 | d2 |";
        let lines = render_markdown(md, 80, &theme());
        // All content must be present.
        let text = all_text(&lines);
        assert!(text.contains("r1"), "missing r1: {:?}", text);
        assert!(text.contains("r2"), "missing r2: {:?}", text);
        // Exactly 2 ├ lines: header sep + 1 body sep.
        let mid_lines: Vec<_> = lines.iter()
            .filter(|l| line_text(l).contains('├'))
            .collect();
        assert_eq!(mid_lines.len(), 2,
            "expected 2 separator lines (header + body), got {}", mid_lines.len());
    }

    #[test]
    fn rm_table_body_sep_lighter_than_header_sep() {
        // The body separator must use a lighter color than the header separator.
        let md = "| A | B |\n\
                  | -- | -- |\n\
                  | r1 | d1 |\n\
                  | -- | -- |\n\
                  | r2 | d2 |";
        let lines = render_markdown(md, 80, &theme());
        let mid_lines: Vec<_> = lines.iter()
            .filter(|l| line_text(l).contains('├'))
            .collect();
        assert_eq!(mid_lines.len(), 2);
        let fg = |l: &&ratatui::text::Line<'static>| {
            l.spans.first().and_then(|s| s.style.fg)
        };
        let header_sep_fg = fg(&mid_lines[0]);
        let body_sep_fg   = fg(&mid_lines[1]);
        assert_eq!(header_sep_fg, Some(Color::DarkGray),
            "header separator should be DarkGray");
        assert_eq!(body_sep_fg, Some(Color::Indexed(244)),
            "body separator should be mid-gray (Indexed 244)");
        assert_ne!(header_sep_fg, body_sep_fg,
            "header and body separators must use different colors");
    }

    #[test]
    fn rm_table_trailing_delimiter_not_drawn() {
        // A delimiter row at the very end of the table (after the last data
        // row) should be silently dropped — no separator before the bottom border.
        let md = "| A | B |\n\
                  | -- | -- |\n\
                  | r1 | d1 |\n\
                  | -- | -- |";
        let lines = render_markdown(md, 80, &theme());
        // Still only 1 ├ (header sep); the trailing delimiter produces none.
        let mid_lines: Vec<_> = lines.iter()
            .filter(|l| line_text(l).contains('├'))
            .collect();
        assert_eq!(mid_lines.len(), 1,
            "trailing delimiter row should not add a separator, got {}", mid_lines.len());
    }

    #[test]
    fn is_separator_row_recognises_common_patterns() {
        use super::is_separator_row;
        assert!(is_separator_row(&["--".into(), "---".into()]));
        assert!(is_separator_row(&[":--:".into(), "---:".into()]));
        assert!(is_separator_row(&[" -- ".into()]));   // leading/trailing spaces trimmed
        assert!(!is_separator_row(&[]));               // empty row
        assert!(!is_separator_row(&["data".into()]));  // non-delimiter cell
        assert!(!is_separator_row(&["--".into(), "data".into()])); // mixed
    }

    // ── Token::Join / punctuation gluing ────────────────────────────────────

    #[test]
    fn reflow_join_suppresses_space_before_next_word() {
        // Token::Join between two words must produce no space.
        let tokens = vec![
            Token::Word("hello".into(), Style::default()),
            Token::Join,
            Token::Word(",".into(), Style::default()),
            Token::Word("world".into(), Style::default()),
        ];
        let lines = reflow(&tokens, 80, "  ", "  ", Style::default());
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "  hello, world");
    }

    #[test]
    fn reflow_join_at_start_is_harmless() {
        // A Join before the very first word must not produce a leading space.
        let tokens = vec![
            Token::Join,
            Token::Word("hello".into(), Style::default()),
        ];
        let lines = reflow(&tokens, 80, "  ", "  ", Style::default());
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "  hello");
    }

    #[test]
    fn reflow_join_reset_after_break() {
        // A Join followed by a Break and then a word must not suppress the
        // space on the new line (there is none anyway, but the flag must clear).
        let tokens = vec![
            Token::Word("a".into(), Style::default()),
            Token::Join,
            Token::Break,
            Token::Word("b".into(), Style::default()),
            Token::Word("c".into(), Style::default()),
        ];
        let lines = reflow(&tokens, 80, "  ", "  ", Style::default());
        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "  a");
        assert_eq!(line_text(&lines[1]), "  b c"); // space between b and c
    }

    #[test]
    fn rm_bold_before_comma_no_space() {
        // `**hello**, world` → must render as "hello, world" (no space before comma).
        let text = all_text(&render_markdown("**hello**, world", 80, &theme()));
        assert!(text.contains("hello,"),
            "expected \"hello,\" (no space), got: {:?}", text);
        assert!(text.contains("hello, world"),
            "expected full \"hello, world\", got: {:?}", text);
    }

    #[test]
    fn rm_punctuation_sandwiched_between_bold() {
        // `,**foo**,**bar**,` → ",foo,bar," with no extra spaces.
        let text = all_text(&render_markdown(",**foo**,**bar**,", 80, &theme()));
        assert!(text.contains(",foo,bar,"),
            "expected \",foo,bar,\" (no spaces), got: {:?}", text);
    }

    #[test]
    fn rm_italic_before_punctuation_no_space() {
        let text = all_text(&render_markdown("*hello*.", 80, &theme()));
        assert!(text.contains("hello."),
            "expected \"hello.\" (no space), got: {:?}", text);
    }

    #[test]
    fn rm_code_span_before_punctuation_no_space() {
        let text = all_text(&render_markdown("`foo()`, bar", 80, &theme()));
        assert!(text.contains("`foo()`,"),
            "expected backtick-foo followed directly by comma, got: {:?}", text);
    }

    #[test]
    fn rm_space_before_markup_preserved() {
        // Explicit space before `**` must still produce a space in output.
        let text = all_text(&render_markdown("hello **world**", 80, &theme()));
        assert!(text.contains("hello world"),
            "expected space between hello and world, got: {:?}", text);
    }

    #[test]
    fn rm_space_after_markup_preserved() {
        // Explicit space after `**` must still produce a space in output.
        let text = all_text(&render_markdown("**hello** world", 80, &theme()));
        assert!(text.contains("hello world"),
            "expected space between hello and world, got: {:?}", text);
    }
}
