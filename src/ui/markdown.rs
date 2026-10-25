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
//! • > Blockquotes  (│ prefix, dark-gray text; lists, headings, code
//!   blocks and rules inside a blockquote keep the bar)
//! • Unordered lists  (• bullet, indented per nesting level)
//! • Ordered lists    (1. 2. …, indented per nesting level)
//! • Tables           (box-drawing borders between *every* row, auto column
//!                    widths; cells may contain inline styles, and `<br>`-
//!                    separated cell lines may start with `#`, `>`, `-`/`*`/
//!                    `+`, `1.`, a `---` rule or a ``` fence to get the
//!                    corresponding block formatting inside the cell)
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

// ─── Table cell model ─────────────────────────────────────────────────────────

/// A run of identically-styled text inside a table cell.
#[derive(Clone, Debug, PartialEq)]
struct CellRun {
    text:  String,
    style: Style,
}

/// One logical line inside a table cell, before wrapping to the column width.
///
/// `first_pfx` is placed in front of the first display line of the cell line,
/// `cont_pfx` in front of every wrapped continuation line.  They carry the
/// block-level decoration (`• `, `1. `, `│ `, `┃ `) and are already styled.
#[derive(Clone, Debug, Default)]
struct CellLine {
    first_pfx: Vec<CellRun>,
    cont_pfx:  Vec<CellRun>,
    runs:      Vec<CellRun>,
    /// Pre-formatted content: preserve whitespace and char-wrap instead of
    /// word-wrapping (used inside `<pre>…</pre>` and `~~~` fences).
    pre:       bool,
    /// A horizontal rule that fills the cell width.
    rule:      bool,
}

/// A raw logical line of a table cell, exactly as it came out of the event
/// stream (one `<br>`-separated chunk).
#[derive(Clone, Debug, Default)]
struct RawCellLine {
    runs: Vec<CellRun>,
    pre:  bool,
}

/// A table cell: a sequence of logical lines.
type Cell = Vec<CellLine>;

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
    table_head_rows:      Vec<Vec<Cell>>,
    table_body_rows:      Vec<Vec<Cell>>,
    current_row:          Vec<Cell>,
    /// Logical lines of the cell currently being parsed (raw, before block
    /// markers such as `#`, `>`, `- ` or ``` ``` ``` are interpreted).
    cell_lines:           Vec<RawCellLine>,
    /// The logical line of the current cell that is still being built.
    cell_line:            Vec<CellRun>,
    /// Nesting depth of `<pre>…</pre>` inside the current cell.
    cell_pre:             usize,
    /// Whether the current cell line started inside a `<pre>` block.
    cell_line_pre:        bool,
    // HTML inline-style toggles inside table cells (`<b>`, `<i>`, `<code>`, …).
    cell_html_bold:       bool,
    cell_html_italic:     bool,
    cell_html_strike:     bool,
    cell_html_code:       bool,

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
            current_row:          vec![],
            cell_lines:           vec![],
            cell_line:            vec![],
            cell_pre:             0,
            cell_line_pre:        false,
            cell_html_bold:       false,
            cell_html_italic:     false,
            cell_html_strike:     false,
            cell_html_code:       false,
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
        let head_rows  = std::mem::take(&mut self.table_head_rows);
        let body_rows  = std::mem::take(&mut self.table_body_rows);
        let alignments = std::mem::take(&mut self.table_alignments);

        let indent = "  ";

        let col_count = head_rows.iter().chain(body_rows.iter())
            .map(|r| r.len())
            .max()
            .unwrap_or(0);
        if col_count == 0 { return; }

        // Per-column minimum width = the widest word (or, for pre-formatted
        // lines, the widest unbreakable chunk) in any cell of that column.
        // A column scaled below this value will have to break at least one
        // word, so it acts as a floor during proportional shrinking.
        let max_word_w: Vec<usize> = (0..col_count).map(|ci| {
            head_rows.iter().chain(body_rows.iter())
                .filter_map(|row| row.get(ci))
                .flat_map(|cell| cell.iter())
                .flat_map(cell_line_chunks)
                .map(|chunk| display_width(&chunk))
                .max()
                .unwrap_or(1)
                .max(1)
        }).collect();

        // Natural column widths (at least 1 char each).
        let mut col_w = vec![1usize; col_count];
        for row in head_rows.iter().chain(body_rows.iter()) {
            for (i, cell) in row.iter().enumerate() {
                if i < col_count {
                    col_w[i] = col_w[i].max(natural_cell_width(cell));
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
                // proportionally from the floors, accepting word breaks.
                for (i, w) in col_w.iter_mut().enumerate() {
                    let mw = max_word_w[i];
                    *w = if sum_min == 0 { 1 }
                         else { ((mw as f64 * avail as f64 / sum_min as f64).floor() as usize).max(1) };
                }
            } else {
                // Give every column its per-word floor first, then distribute
                // the remaining budget proportionally to each column's natural
                // slack (natural_width − floor).  This prevents word breaks
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

        let bdr_s = self.table_border_style;
        let head_s = self.table_head_style;

        self.output.push(Line::from(Span::styled(top, bdr_s)));

        let all_rows: Vec<(Vec<Cell>, bool)> = head_rows.into_iter().map(|r| (r, true))
            .chain(body_rows.into_iter().map(|r| (r, false)))
            .collect();

        for (row_idx, (row, is_head)) in all_rows.iter().enumerate() {
            // A header row gets the bold base style patched under every cell
            // run, so bold/italic/code inside a header cell compose with it.
            let cell_base = if *is_head { head_s } else { Style::default() };

            // Lay each cell out to its column width.
            let laid_out: Vec<Vec<Vec<Span<'static>>>> = col_w.iter().enumerate().map(|(ci, &w)| {
                let cell = row.get(ci);
                layout_cell(cell, w, cell_base, self.rule_style)
            }).collect();

            let row_height = laid_out.iter().map(Vec::len).max().unwrap_or(1);

            for line_idx in 0..row_height {
                let mut spans: Vec<Span<'static>> = vec![
                    Span::raw(indent.to_string()),
                    Span::styled("│".to_string(), bdr_s),
                ];
                for (ci, &w) in col_w.iter().enumerate() {
                    let cell_spans = laid_out[ci].get(line_idx);
                    let align = alignments.get(ci).copied().unwrap_or(Alignment::None);

                    // One space of padding on each side, plus any slack needed
                    // to satisfy the column alignment.
                    let content_w = cell_spans
                        .map(|s| s.iter().map(|sp| display_width(sp.content.as_ref())).sum::<usize>())
                        .unwrap_or(0);
                    let pad = w.saturating_sub(content_w);
                    let (lpad, rpad) = match align {
                        Alignment::Right  => (pad, 0),
                        Alignment::Center => (pad / 2, pad - pad / 2),
                        _                 => (0, pad),
                    };

                    spans.push(Span::raw(" ".repeat(lpad + 1)));
                    if let Some(cell_spans) = cell_spans {
                        spans.extend(cell_spans.iter().cloned());
                    }
                    spans.push(Span::raw(" ".repeat(rpad + 1)));
                    spans.push(Span::styled("│".to_string(), bdr_s));
                }
                self.output.push(Line::from(spans));
            }

            // Every row is followed by a rule in the border colour — except
            // the last one, which is closed by the bottom border.
            if row_idx + 1 < all_rows.len() {
                self.output.push(Line::from(Span::styled(mid.clone(), bdr_s)));
            }
        }

        self.output.push(Line::from(Span::styled(bot, bdr_s)));
    }

    // ── Table cell parsing ────────────────────────────────────────────────────

    /// Combined style for a run of text inside a table cell: the Markdown
    /// inline toggles (Strong / Emphasis / Strikethrough) merged with the
    /// HTML-style toggles (`<b>`, `<i>`, `<s>`, `<code>`).
    fn cell_style(&self) -> Style {
        let mut s = Style::default();
        if self.bold || self.cell_html_bold {
            s = s.add_modifier(Modifier::BOLD);
        }
        if self.italic || self.cell_html_italic {
            s = s.add_modifier(Modifier::ITALIC);
        }
        if self.strikethrough || self.cell_html_strike {
            s = s.add_modifier(Modifier::CROSSED_OUT);
        }
        if self.cell_html_code {
            s = s.patch(self.code_style);
        }
        s
    }

    /// Append `text` to the cell line currently being built.
    fn cell_push(&mut self, text: &str) {
        if text.is_empty() { return; }
        let style = self.cell_style();
        self.cell_line.push(CellRun { text: text.to_string(), style });
    }

    /// Start a new logical line inside the current cell (a `<br>`).
    fn cell_break(&mut self) {
        let pre = self.cell_line_pre || self.cell_pre > 0;
        let runs = std::mem::take(&mut self.cell_line);
        self.cell_lines.push(RawCellLine { runs, pre });
        self.cell_line_pre = self.cell_pre > 0;
    }

    /// Unfold a code span that pulldown-cmark produced from a ``` fence
    /// inside a table cell into pre-formatted lines.  The span text looks
    /// like `<br>  x = 1<br>  y = 2<br>`.
    fn cell_code_block(&mut self, code: &str) {
        let style = self.code_style;
        // The span wraps the content in <br> markers: "<br>  x = 1<br>".
        // Leading/trailing empty pieces are artifacts of the fences, so drop
        // them rather than rendering blank lines at the top and bottom.
        let pieces: Vec<&str> = code.split("<br>").collect();
        let start = pieces.iter().position(|p| !p.trim().is_empty()).unwrap_or(pieces.len());
        let end   = pieces.iter().rposition(|p| !p.trim().is_empty()).map_or(start, |i| i + 1);

        for (i, piece) in pieces[start..end].iter().enumerate() {
            if i > 0 {
                self.cell_break();
            }
            self.cell_line_pre = true;
            self.cell_line.push(CellRun { text: (*piece).to_string(), style });
        }
    }

    /// Handle an inline HTML tag inside a table cell.
    ///
    /// `pulldown-cmark` does not parse HTML inside table cells, so the tags
    /// arrive verbatim.  We support the ones that are useful in a table:
    /// `<br>` (a line break) and the style tags `<b>`/`<strong>`,
    /// `<i>`/`<em>`, `<s>`/`<del>`, `<code>`, `<pre>`, plus their closers.
    /// Any other tag is dropped.
    fn cell_inline_html(&mut self, tag: &str) {
        let t = tag.trim();
        let (closing, name) = match t.strip_prefix("</") {
            Some(rest) => (true, rest),
            None => (false, t.strip_prefix('<').unwrap_or(t)),
        };
        let name: String = name
            .trim_end_matches('>')
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();

        match name.as_str() {
            "br" => self.cell_break(),
            "b" | "strong" => self.cell_html_bold = !closing,
            "i" | "em" => self.cell_html_italic = !closing,
            "s" | "del" | "strike" => self.cell_html_strike = !closing,
            "code" => self.cell_html_code = !closing,
            "pre" => {
                if closing {
                    self.cell_pre = self.cell_pre.saturating_sub(1);
                } else {
                    self.cell_pre += 1;
                    // Content after `<pre>` starts a fresh pre-formatted line.
                    self.cell_line_pre = true;
                }
            }
            _ => {}
        }
    }

    /// Finish the current cell: split its raw lines on block markers, compute
    /// list numbering, and stash the result in `current_row`.
    fn cell_finish(&mut self) {
        // Flush a trailing partial line.
        let pre = self.cell_line_pre || self.cell_pre > 0;
        let runs = std::mem::take(&mut self.cell_line);
        self.cell_lines.push(RawCellLine { runs, pre });
        self.cell_line_pre = false;

        let raw = std::mem::take(&mut self.cell_lines);
        let cell = build_cell(&raw, self.blockquote_style);
        self.current_row.push(cell);
    }

    // ── Event handler ─────────────────────────────────────────────────────────

    #[allow(clippy::match_same_arms)]
    fn push(&mut self, event: Event<'_>) {
        // While inside a table cell, inline events are collected into the
        // cell's run list; only structural events (cell/row/table ends) fall
        // through to the main match below.
        if self.in_table_cell {
            match &event {
                Event::Text(s) => {
                    self.cell_push(s);
                    return;
                }
                Event::Code(s) => {
                    // pulldown-cmark parses a ```-fenced cell block as a code
                    // span whose text still contains the literal `<br>` tags.
                    // Unfold it into pre-formatted lines so a fence works
                    // inside a table cell.
                    if s.contains("<br>") || s.contains("<br/>") {
                        self.cell_code_block(s);
                    } else {
                        // Inline code keeps its backticks, as in body text.
                        let style = self.cell_style().patch(self.code_style);
                        self.cell_line.push(CellRun { text: format!("`{}`", s), style });
                    }
                    return;
                }
                Event::InlineHtml(s) => {
                    self.cell_inline_html(s);
                    return;
                }
                Event::SoftBreak | Event::HardBreak => {
                    self.cell_push(" ");
                    return;
                }
                // Inline-style markers toggle the shared flags; `cell_style`
                // merges them with the HTML-style flags.
                Event::Start(Tag::Strong)         => { self.bold = true;          return; }
                Event::End(TagEnd::Strong)        => { self.bold = false;         return; }
                Event::Start(Tag::Emphasis)       => { self.italic = true;        return; }
                Event::End(TagEnd::Emphasis)      => { self.italic = false;       return; }
                Event::Start(Tag::Strikethrough)  => { self.strikethrough = true; return; }
                Event::End(TagEnd::Strikethrough) => { self.strikethrough = false; return; }
                Event::Start(Tag::Link { .. }) | Event::End(TagEnd::Link)     => { return; }
                Event::Start(Tag::Image { .. }) | Event::End(TagEnd::Image)   => { return; }
                _ => {} // structural events fall through
            }
        }

        match event {
            // ── Headings ─────────────────────────────────────────────────────
            Event::Start(Tag::Heading { level, .. }) => {
                if self.pending_blank { self.push_blank_in_context(); self.pending_blank = false; }
                self.flush();
                self.blockquote_just_opened = false;
                let (pfx, style) = match level {
                    HeadingLevel::H1 => ("  ", self.h1_style),
                    HeadingLevel::H2 => ("  ", self.h2_style),
                    HeadingLevel::H3 => ("  ", self.h3_style),
                    _                => ("    ", self.h_lower_style),
                };
                // Inside a blockquote the heading keeps its colour/bold but
                // sits behind the │ bar (bar style is the base, so the
                // heading's own colour wins when explicitly set).
                let (pfx, style) = if self.blockquote_depth > 0 {
                    (self.bq_prefix(), self.blockquote_style.patch(style))
                } else {
                    (pfx.to_string(), style)
                };
                self.first_pfx  = pfx.clone();
                self.cont_pfx   = pfx;
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
                if self.pending_blank { self.push_blank_in_context(); self.pending_blank = false; }
                self.blockquote_just_opened = false;
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
                // Inside a blockquote the 2-space margin becomes the │ bar
                // prefix so list items stay inside the quote.
                let depth      = self.list_stack.len(); // ≥ 1
                let base       = if self.blockquote_depth > 0 { self.bq_prefix() } else { "  ".to_string() };
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
                    (format!("{}• ", base), format!("{}  ", base))
                };
                self.item_first_pfx       = first.clone();
                self.item_cont_pfx        = cont.clone();
                self.first_pfx            = first;
                self.cont_pfx             = cont;
                self.in_item              = true;
                self.item_paragraph_count = 0;
                self.line_style = if self.blockquote_depth > 0 {
                    self.blockquote_style
                } else {
                    Style::default()
                };
            }
            Event::End(TagEnd::Item) => {
                // Tight list items put their text directly in the item (no
                // Start(Paragraph)); flush whatever was accumulated.
                self.flush();
                self.in_item = false;
            }

            // ── Code blocks ──────────────────────────────────────────────────
            Event::Start(Tag::CodeBlock(_)) => {
                if self.pending_blank { self.push_blank_in_context(); self.pending_blank = false; }
                else { self.push_blank_in_context(); }
                self.blockquote_just_opened = false;
                self.in_code_block = true;
                self.code_buffer.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                self.in_code_block = false;
                let code  = std::mem::take(&mut self.code_buffer);
                // Left edge: the │ bar inside a blockquote, plain indent otherwise.
                let pfx   = if self.blockquote_depth > 0 { self.bq_prefix() } else { "  ".to_string() };
                // Reserve space for prefix + "┃ " + at least 1 char of content.
                let max_w = self.width.saturating_sub(display_width(&pfx) + 3).max(1);
                let cs    = self.code_style;
                let bs    = self.blockquote_style;
                for raw_line in code.lines() {
                    for chunk in char_wrap_line(raw_line, max_w) {
                        self.output.push(Line::from(vec![
                            Span::styled(pfx.clone(), bs),
                            Span::styled("┃ ", Style::default().fg(Color::Yellow)),
                            Span::styled(chunk, cs),
                        ]));
                    }
                }
                // An empty code block produces no output; in either case close
                // with a blank so the next block has breathing room.
                self.push_blank_in_context();
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
                self.current_row.clear();
                self.cell_lines.clear();
                self.cell_line.clear();
                self.cell_pre = 0;
                self.cell_line_pre = false;
                self.cell_html_bold = false;
                self.cell_html_italic = false;
                self.cell_html_strike = false;
                self.cell_html_code = false;
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
                self.table_body_rows.push(row);
            }
            Event::Start(Tag::TableCell)  => {
                self.cell_lines.clear();
                self.cell_line.clear();
                self.cell_pre = 0;
                self.cell_line_pre = false;
                self.cell_html_bold = false;
                self.cell_html_italic = false;
                self.cell_html_strike = false;
                self.cell_html_code = false;
                self.in_table_cell = true;
            }
            Event::End(TagEnd::TableCell) => {
                self.in_table_cell = false;
                self.cell_finish();
            }

            // ── Horizontal rule ──────────────────────────────────────────────
            Event::Rule => {
                if self.pending_blank { self.push_blank_in_context(); self.pending_blank = false; }
                self.blockquote_just_opened = false;
                let bar = if self.blockquote_depth > 0 { self.bq_prefix() } else { "  ".to_string() };
                let rule_w = self.width.saturating_sub(display_width(&bar) + 2).max(1);
                self.output.push(Line::from(Span::styled(
                    format!("{}{}", bar, "─".repeat(rule_w)),
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

// ─── Table cell helpers ─────────────────────────────────────────────────────

/// Style of a heading of the given level, mirroring the body-text headings.
fn heading_style(level: u8) -> Style {
    let colour = match level {
        1 => Color::Indexed(20),
        2 => Color::Indexed(26),
        3 => Color::Indexed(62),
        _ => return Style::default().add_modifier(Modifier::BOLD),
    };
    Style::default().fg(colour).add_modifier(Modifier::BOLD)
}

/// Patch `base` underneath every run's own style, so a block-level style
/// (heading colour, quote grey) carries through while inline styles still win.
fn restyle(runs: &[CellRun], base: Style) -> Vec<CellRun> {
    runs.iter()
        .map(|r| CellRun { text: r.text.clone(), style: base.patch(r.style) })
        .collect()
}

/// Strip `runs` down to their concatenated plain text.
fn runs_text(runs: &[CellRun]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

/// Split `runs` into styled chunks at every whitespace boundary.
///
/// Each returned `(chunk, style)` pair is a candidate word for reflow.  The
/// separating whitespace is *not* included in any chunk, but the caller can
/// tell whether whitespace preceded a chunk from its position.  Because a
/// word may straddle several runs (`**bo**ld`), adjacent runs without an
/// intervening space are merged into one chunk carrying the first run's
/// style.
fn runs_words(runs: &[CellRun]) -> Vec<(String, Style)> {
    let mut out: Vec<(String, Style)> = vec![];
    let mut cur = String::new();
    let mut cur_style = Style::default();
    let mut have = false;

    for run in runs {
        for ch in run.text.chars() {
            if ch.is_whitespace() {
                if have {
                    out.push((std::mem::take(&mut cur), cur_style));
                    have = false;
                }
            } else {
                if !have {
                    cur_style = run.style;
                    have = true;
                }
                cur.push(ch);
            }
        }
    }
    if have {
        out.push((cur, cur_style));
    }
    out
}

/// The unbreakable chunks of a cell line, used to compute a column's minimum
/// width.  For pre-formatted lines the chunks are whole (whitespace-included)
/// segments between double spaces, since leading indentation must survive.
fn cell_line_chunks(line: &CellLine) -> Vec<String> {
    let text = runs_text(&line.runs);
    if line.pre {
        // Pre-formatted: only break at runs of 2+ spaces, keeping indentation.
        text.split("  ")
            .map(str::to_string)
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        text.split_whitespace().map(str::to_string).collect()
    }
}

/// Natural (unwrapped) width of a cell: the widest of its logical lines,
/// including the width of the line's prefix.
fn natural_cell_width(cell: &Cell) -> usize {
    cell.iter()
        .map(|line| {
            let pfx = display_width(&runs_text(&line.first_pfx));
            let body = if line.rule {
                1
            } else {
                display_width(&runs_text(&line.runs))
            };
            pfx + body
        })
        .max()
        .unwrap_or(0)
}

// ─── Table cell block parsing ───────────────────────────────────────────────

/// How a raw cell line is interpreted.
enum CellBlock {
    /// Plain paragraph text (default).
    Text,
    /// `# …`, `## …` … — heading of the given level.
    Heading(u8),
    /// `> …` — blockquote, possibly nested (`>> …`).
    Quote(usize),
    /// `- …` / `* …` / `+ …` — unordered list item at the given depth.
    Bullet(usize),
    /// `1. …` — ordered list item.
    Ordered(u64, usize),
    /// `---` — a horizontal rule spanning the cell.
    Rule,
    /// A ``` / ~~~ fence delimiter.
    Fence,
}

/// Recognise a leading block marker in `text`, returning the block kind and
/// the remaining content (already trimmed of the marker's trailing space).
fn parse_block_marker(text: &str) -> (CellBlock, &str) {
    let trimmed = text.trim_start();
    let indent  = (text.len() - trimmed.len()) / 2; // 2 spaces ≈ 1 nesting level

    // Horizontal rule.
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() >= 3
        && compact.chars().all(|c| c == '-')
    {
        return (CellBlock::Rule, "");
    }

    // Fences.
    if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
        return (CellBlock::Fence, "");
    }

    // Blockquote (nested bars).
    let mut bars = 0;
    let mut rest = trimmed;
    while let Some(r) = rest.strip_prefix('>') {
        bars += 1;
        rest = r.strip_prefix(' ').unwrap_or(r);
    }
    if bars > 0 {
        return (CellBlock::Quote(bars + indent), rest);
    }

    // Heading.
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) {
        let after = &trimmed[hashes..];
        if after.is_empty() || after.starts_with(' ') {
            return (CellBlock::Heading(hashes as u8), after.trim_start());
        }
    }

    // Unordered list item.
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(marker) {
            return (CellBlock::Bullet(indent + 1), rest);
        }
    }

    // Ordered list item.
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(rest) = trimmed[digits..].strip_prefix(". ") {
            if let Ok(n) = trimmed[..digits].parse::<u64>() {
                return (CellBlock::Ordered(n, indent + 1), rest);
            }
        }
    }

    (CellBlock::Text, text)
}

/// Turn the raw `<br>`-separated lines of a cell into laid-out [`CellLine`]s,
/// interpreting block markers and tracking ordered-list numbering.
fn build_cell(raw: &[RawCellLine], quote_style: Style) -> Cell {
    let mut out: Vec<CellLine> = vec![];
    // Next number for each ordered-list nesting depth.
    let mut counters: Vec<u64> = vec![];
    // Whether we are inside a ``` fence.
    let mut in_fence = false;

    for raw_line in raw {
        let text = runs_text(&raw_line.runs);

        // Inside a fence: emit verbatim pre-formatted lines until the closing
        // fence, without interpreting any block marker.
        if in_fence {
            if text.trim_start().starts_with("```") || text.trim_start().starts_with("~~~") {
                in_fence = false;
                continue;
            }
            out.push(CellLine {
                first_pfx: vec![],
                cont_pfx:  vec![],
                runs:      raw_line.runs.clone(),
                pre:       true,
                rule:      false,
            });
            continue;
        }

        // Lines that were inside `<pre>…</pre>` are already pre-formatted.
        if raw_line.pre {
            out.push(CellLine {
                first_pfx: vec![],
                cont_pfx:  vec![],
                runs:      raw_line.runs.clone(),
                pre:       true,
                rule:      false,
            });
            continue;
        }

        let (block, rest) = parse_block_marker(&text);

        match block {
            CellBlock::Fence => { in_fence = true; }

            CellBlock::Rule => out.push(CellLine {
                first_pfx: vec![],
                cont_pfx:  vec![],
                runs:      vec![],
                pre:       false,
                rule:      true,
            }),

            CellBlock::Heading(level) => {
                counters.clear();
                let h_style = heading_style(level);
                let pfx = vec![CellRun { text: format!("{} ", "#".repeat(level as usize)), style: h_style }];
                out.push(CellLine {
                    first_pfx: pfx.clone(),
                    cont_pfx:  pfx,
                    runs:      restyle(&slice_runs_after(&raw_line.runs, &text, rest), h_style),
                    pre:       false,
                    rule:      false,
                });
            }

            CellBlock::Quote(depth) => {
                counters.clear();
                let pfx = vec![CellRun { text: format!("{} ", "│".repeat(depth)), style: quote_style }];
                out.push(CellLine {
                    first_pfx: pfx.clone(),
                    cont_pfx:  pfx,
                    runs:      restyle(&slice_runs_after(&raw_line.runs, &text, rest), quote_style),
                    pre:       false,
                    rule:      false,
                });
            }

            CellBlock::Bullet(depth) => {
                // A bullet ends any ordered list nested at or below its depth.
                counters.truncate(depth.saturating_sub(1).min(counters.len()));
                let indent = "  ".repeat(depth.saturating_sub(1));
                let pfx = vec![CellRun { text: format!("{indent}• "), style: Style::default() }];
                out.push(CellLine {
                    first_pfx: pfx,
                    cont_pfx:  vec![CellRun { text: format!("{indent}  "), style: Style::default() }],
                    runs:      slice_runs_after(&raw_line.runs, &text, rest),
                    pre:       false,
                    rule:      false,
                });
            }

            CellBlock::Ordered(n, depth) => {
                // `counters[d]` is the number of the most recent ordered item
                // at depth `d + 1`.  An item at a depth we have not seen yet
                // *starts* a list there, so honour the number the author wrote;
                // otherwise continue counting from the previous item, which
                // makes `1.` / `1.` / `1.` render as 1, 2, 3.
                let num = if counters.len() >= depth {
                    // Deeper lists have ended.
                    counters.truncate(depth);
                    counters[depth - 1] += 1;
                    counters[depth - 1]
                } else {
                    while counters.len() < depth - 1 { counters.push(1); }
                    counters.push(n);
                    n
                };
                let indent = "  ".repeat(depth.saturating_sub(1));
                let bullet = format!("{indent}{num}. ");
                out.push(CellLine {
                    first_pfx: vec![CellRun { text: bullet.clone(), style: Style::default() }],
                    cont_pfx:  vec![CellRun { text: " ".repeat(bullet.len()), style: Style::default() }],
                    runs:      slice_runs_after(&raw_line.runs, &text, rest),
                    pre:       false,
                    rule:      false,
                });
            }

            CellBlock::Text => {
                counters.clear();
                out.push(CellLine {
                    first_pfx: vec![],
                    cont_pfx:  vec![],
                    runs:      raw_line.runs.clone(),
                    pre:       false,
                    rule:      false,
                });
            }
        }
    }

    out
}

/// Return the runs of `runs` with the first `skip` visible characters
/// removed, so block markers can be stripped without losing inline styling.
fn slice_runs_after(runs: &[CellRun], full: &str, rest: &str) -> Vec<CellRun> {
    // Number of characters the marker consumed.  Counting in `char`s (not
    // bytes) keeps this correct for non-ASCII cell content.
    let consumed = full.chars().count().saturating_sub(rest.chars().count());
    if consumed == 0 {
        return runs.to_vec();
    }
    let mut remaining = consumed;
    let mut out = vec![];
    for run in runs {
        let n = run.text.chars().count();
        if remaining >= n {
            remaining -= n;
            continue;
        }
        let kept: String = run.text.chars().skip(remaining).collect();
        remaining = 0;
        if !kept.is_empty() {
            out.push(CellRun { text: kept, style: run.style });
        }
    }
    out
}

// ─── Table cell layout ──────────────────────────────────────────────────────

/// Word-wrap `text` into lines of at most `width` visible characters,
/// breaking on whitespace boundaries.  Words longer than `width` are
/// hard-wrapped character by character.  Always returns at least one element.
///
/// This is the plain-text entry point into the cell layout machinery; the
/// renderer itself works on styled [`CellLine`]s, so this wrapper exists only
/// to keep the wrapping behaviour directly testable.
#[cfg(test)]
fn word_wrap_cell(text: &str, width: usize) -> Vec<String> {
    let line = CellLine {
        first_pfx: vec![],
        cont_pfx:  vec![],
        runs:      vec![CellRun { text: text.to_string(), style: Style::default() }],
        pre:       false,
        rule:      false,
    };
    let laid_out = wrap_cell_line(&line, width.max(1), Style::default(), Style::default());
    let mut out: Vec<String> = laid_out
        .iter()
        .map(|spans| spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    // Trim trailing padding introduced by wrapping, and guarantee the
    // "at least one element" contract for empty input.
    while out.len() > 1 && out.last().is_some_and(|s| s.trim().is_empty()) {
        out.pop();
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Wrap one cell's runs into lines of at most `width` columns, prefixing the
/// first line with `first_pfx` and continuations with `cont_pfx`.
fn wrap_cell_line(
    line:   &CellLine,
    width:  usize,
    base:   Style,
    rule_s: Style,
) -> Vec<Vec<Span<'static>>> {
    let mut out: Vec<Vec<Span<'static>>> = vec![];

    if line.rule {
        out.push(vec![Span::styled("─".repeat(width), rule_s)]);
        return out;
    }

    let first_pfx_w = display_width(&runs_text(&line.first_pfx));
    let cont_pfx_w  = display_width(&runs_text(&line.cont_pfx));

    // Prefix runs, with `base` patched underneath.
    let emit_pfx = |pfx: &[CellRun]| -> Vec<Span<'static>> {
        pfx.iter()
            .map(|r| Span::styled(r.text.clone(), base.patch(r.style)))
            .collect()
    };

    let mut spans = emit_pfx(&line.first_pfx);
    let mut cur_w = first_pfx_w;
    let mut words_on_line = 0usize;

    if line.pre {
        // Pre-formatted: preserve whitespace; char-wrap at the column edge.
        for run in &line.runs {
            let mut chunk = String::new();
            for ch in run.text.chars() {
                let cw = char_display_width(ch);
                if cur_w + cw > width && cur_w > 0 {
                    if !chunk.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut chunk), base.patch(run.style)));
                    }
                    out.push(std::mem::take(&mut spans));
                    spans = emit_pfx(&line.cont_pfx);
                    cur_w = cont_pfx_w;
                }
                chunk.push(ch);
                cur_w += cw;
            }
            if !chunk.is_empty() {
                spans.push(Span::styled(chunk, base.patch(run.style)));
            }
        }
        out.push(spans);
        return out;
    }

    let words = runs_words(&line.runs);
    for (word, style) in words {
        let ww = display_width(&word);
        let space = usize::from(words_on_line > 0);

        if words_on_line > 0 && cur_w + space + ww > width {
            out.push(std::mem::take(&mut spans));
            spans = emit_pfx(&line.cont_pfx);
            cur_w = cont_pfx_w;
            words_on_line = 0;
        } else if space > 0 {
            spans.push(Span::styled(" ", Style::default()));
            cur_w += 1;
        }

        // Hard-break a word that cannot fit on the remaining space.
        if cur_w + ww > width {
            for ch in word.chars() {
                let cw = char_display_width(ch);
                if cur_w + cw > width && cur_w > 0 {
                    out.push(std::mem::take(&mut spans));
                    spans = emit_pfx(&line.cont_pfx);
                    cur_w = cont_pfx_w;
                }
                spans.push(Span::styled(ch.to_string(), base.patch(style)));
                cur_w += cw;
            }
        } else {
            spans.push(Span::styled(word, base.patch(style)));
            cur_w += ww;
        }
        words_on_line += 1;
    }

    out.push(spans);
    out
}

/// Lay out a whole cell to `width` columns, returning one `Vec<Span>` per
/// display line.
fn layout_cell(
    cell:  Option<&Cell>,
    width: usize,
    base:  Style,
    rule_s: Style,
) -> Vec<Vec<Span<'static>>> {
    let Some(cell) = cell else {
        return vec![vec![]];
    };
    if cell.is_empty() {
        return vec![vec![]];
    }
    let mut out = vec![];
    for line in cell {
        out.extend(wrap_cell_line(line, width, base, rule_s));
    }
    out
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
    fn rm_blockquote_contains_list() {
        // Lists inside a blockquote must stay behind the │ bar.
        let md = "> Para\n>\n> - uno\n> - dos\n>\n> 1. one\n> 2. two";
        let lines = render_markdown(md, 80, &theme());
        for line in &lines {
            if line.spans.is_empty() { continue; }
            let text = line_text(line);
            assert!(text.contains('│'), "line missing │ bar: {:?}", text);
        }
        let full = all_text(&lines);
        assert!(full.contains("•"),  "no bullet: {:?}", full);
        assert!(full.contains("uno"), "got: {:?}", full);
        assert!(full.contains("dos"), "got: {:?}", full);
        assert!(full.contains("1."), "got: {:?}", full);
        assert!(full.contains("two"), "got: {:?}", full);
        // List words carry the blockquote style.
        let span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content.contains("uno")).unwrap();
        assert_eq!(span.style.fg, Some(Color::DarkGray));
    }

    #[test]
    fn rm_blockquote_contains_heading() {
        // A heading inside a blockquote keeps its colour but sits behind the
        // bar and doesn't break out of the quote.
        let md = "> Para\n>\n> # Head 1\n>\n> More";
        let lines = render_markdown(md, 80, &theme());
        for line in &lines {
            if line.spans.is_empty() { continue; }
            let text = line_text(line);
            assert!(text.contains('│'), "line missing │ bar: {:?}", text);
        }
        let span = lines.iter().flat_map(|l| &l.spans)
            .find(|s| s.content.contains("Head")).unwrap();
        // Heading colour (H1 navy) survives; bar prefix is a separate span.
        assert_eq!(span.style.fg, Some(Color::Indexed(20)));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn rm_blockquote_contains_code_block() {
        // A fenced code block inside a blockquote keeps the │ bar on every
        // line (including the ┃ gutter lines).
        let md = "> Para\n>\n> ```\n> pre line\n> ```\n>\n> After";
        let lines = render_markdown(md, 80, &theme());
        for line in &lines {
            if line.spans.is_empty() { continue; }
            let text = line_text(line);
            assert!(text.contains('│'), "line missing │ bar: {:?}", text);
        }
        let full = all_text(&lines);
        assert!(full.contains("pre line"), "got: {:?}", full);
        assert!(full.contains("After"),    "got: {:?}", full);
        assert!(full.contains("┃"),        "no ┃ gutter: {:?}", full);
    }

    #[test]
    fn rm_blockquote_sample_document_end_to_end() {
        // The full sample document: mixed content inside one contiguous quote.
        let md = concat!(
            "> Hello\n",
            ">\n",
            "> - unordered\n",
            "> - list\n",
            ">\n",
            "> 1. ordered\n",
            "> 2. list\n",
            ">\n",
            "> **bold**\n",
            "> _italic_\n",
            "> # heading 1\n",
            ">\n",
            "> `code`\n",
            ">\n",
            "> ```\n",
            "> pre\n",
            "> ```\n",
            ">\n",
            "> Closing",
        );
        let lines = render_markdown(md, 80, &theme());
        for line in &lines {
            if line.spans.is_empty() { continue; }
            let text = line_text(line);
            assert!(text.contains('│'),
                "blockquote broken (line without bar): {:?}", text);
        }
        let full = all_text(&lines);
        for needle in ["Hello", "unordered", "1.", "bold", "heading", "`code`", "pre", "Closing"] {
            assert!(full.contains(needle), "missing {:?}: {:?}", needle, full);
        }
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

    // ── Inter-row borders ────────────────────────────────────────────────────

    #[test]
    fn rm_table_draws_rule_between_every_row() {
        // Every pair of adjacent rows is separated by a ├ rule, so an
        // n-row table has exactly n−1 of them.
        let md = "| A | B |\n| -- | -- |\n| r1 | d1 |\n| r2 | d2 |";
        let lines = render_markdown(md, 80, &theme());
        let mid_lines = lines.iter().filter(|l| line_text(l).contains('├')).count();
        assert_eq!(mid_lines, 2, "3 rows → 2 rules (header|r1, r1|r2)");
    }

    #[test]
    fn rm_table_single_body_row_has_header_rule_only() {
        let md = "| A | B |\n| -- | -- |\n| r1 | d1 |";
        let lines = render_markdown(md, 80, &theme());
        let mid_lines = lines.iter().filter(|l| line_text(l).contains('├')).count();
        assert_eq!(mid_lines, 1, "2 rows → 1 rule");
    }

    #[test]
    fn rm_table_rules_use_border_colour() {
        // Inter-row rules must be the same colour as the outer borders.
        let md = "| A | B |\n| -- | -- |\n| r1 | d1 |\n| r2 | d2 |";
        let lines = render_markdown(md, 80, &theme());
        let fg_of = |pred: fn(&str) -> bool| -> Option<Color> {
            lines.iter()
                .find(|l| pred(&line_text(l)))
                .and_then(|l| l.spans.first())
                .and_then(|s| s.style.fg)
        };
        let border_fg = fg_of(|t| t.contains('┌'));
        let mid_fg    = fg_of(|t| t.contains('├'));
        assert_eq!(border_fg, Some(Color::DarkGray));
        assert_eq!(mid_fg, border_fg, "inter-row rules must match the border colour");
    }

    #[test]
    fn rm_table_repeated_delimiter_is_ordinary_row() {
        // The old "optional divider" syntax is gone: a repeated delimiter row
        // is now just a normal data row, so it renders as content and does
        // not add an extra rule beyond the always-on inter-row ones.
        let md = "| A | B |\n\
                  | -- | -- |\n\
                  | r1 | d1 |\n\
                  | -- | -- |\n\
                  | r2 | d2 |";
        let lines = render_markdown(md, 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("r1") && text.contains("r2"), "got: {:?}", text);
        // 4 rows → 3 rules.
        let mid_lines = lines.iter().filter(|l| line_text(l).contains('├')).count();
        assert_eq!(mid_lines, 3, "4 rows → 3 rules, got {}", mid_lines);
    }

    // ── Table cell inline styles ─────────────────────────────────────────────

    /// Find the span in `lines` that contains `needle`.
    fn span_with<'a>(lines: &'a [Line<'static>], needle: &str) -> &'a ratatui::text::Span<'static> {
        lines.iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.contains(needle))
            .unwrap_or_else(|| panic!("no span containing {:?} in {}", needle, all_text(lines)))
    }

    #[test]
    fn rm_table_cell_bold() {
        let md = "| A |\n| -- |\n| plain **bold** |";
        let lines = render_markdown(md, 80, &theme());
        let s = span_with(&lines, "bold");
        assert!(s.style.add_modifier.contains(Modifier::BOLD),
            "expected BOLD, got {:?}", s.style);
        assert!(!s.content.contains("**"), "markers must be stripped: {:?}", s.content);
    }

    #[test]
    fn rm_table_cell_italic() {
        let md = "| A |\n| -- |\n| plain *italic* |";
        let lines = render_markdown(md, 80, &theme());
        let s = span_with(&lines, "italic");
        assert!(s.style.add_modifier.contains(Modifier::ITALIC),
            "expected ITALIC, got {:?}", s.style);
        assert!(!s.content.contains('*'), "markers must be stripped: {:?}", s.content);
    }

    #[test]
    fn rm_table_cell_strikethrough() {
        let md = "| A |\n| -- |\n| ~~gone~~ |";
        let lines = render_markdown(md, 80, &theme());
        let s = span_with(&lines, "gone");
        assert!(s.style.add_modifier.contains(Modifier::CROSSED_OUT),
            "expected CROSSED_OUT, got {:?}", s.style);
    }

    #[test]
    fn rm_table_cell_inline_code() {
        let md = "| A |\n| -- |\n| use `cargo` now |";
        let lines = render_markdown(md, 80, &theme());
        let s = span_with(&lines, "cargo");
        assert_eq!(s.content.as_ref(), "`cargo`", "code keeps its backticks");
        assert_eq!(s.style.fg, Some(Color::DarkGray), "code style colour expected");
    }

    #[test]
    fn rm_table_cell_nested_styles_compose() {
        let md = "| A |\n| -- |\n| ***both*** |";
        let lines = render_markdown(md, 80, &theme());
        let s = span_with(&lines, "both");
        assert!(s.style.add_modifier.contains(Modifier::BOLD));
        assert!(s.style.add_modifier.contains(Modifier::ITALIC));
    }

    #[test]
    fn rm_table_header_cell_keeps_styles_and_bold() {
        let md = "| **Head** |\n| -- |\n| x |";
        let lines = render_markdown(md, 80, &theme());
        let s = span_with(&lines, "Head");
        assert!(s.style.add_modifier.contains(Modifier::BOLD));
        assert!(!s.content.contains('*'));
    }

    #[test]
    fn rm_table_cell_html_bold_and_italic() {
        let md = "| A |\n| -- |\n| <b>bb</b> <i>ii</i> |";
        let lines = render_markdown(md, 80, &theme());
        assert!(span_with(&lines, "bb").style.add_modifier.contains(Modifier::BOLD));
        assert!(span_with(&lines, "ii").style.add_modifier.contains(Modifier::ITALIC));
        let text = all_text(&lines);
        assert!(!text.contains("<b>") && !text.contains("<i>"), "tags leaked: {:?}", text);
    }

    // ── Table cell block elements ────────────────────────────────────────────

    #[test]
    fn rm_table_cell_br_splits_lines() {
        let md = "| A |\n| -- |\n| one<br>two |";
        let lines = render_markdown(md, 80, &theme());
        let cell_rows: Vec<String> = lines.iter()
            .map(|l| line_text(l))
            .filter(|t| t.contains('│'))
            .collect();
        assert!(cell_rows.iter().any(|t| t.contains("one")), "{:?}", cell_rows);
        assert!(cell_rows.iter().any(|t| t.contains("two")), "{:?}", cell_rows);
        assert!(!cell_rows.iter().any(|t| t.contains("one") && t.contains("two")),
            "'one' and 'two' must be on separate rows: {:?}", cell_rows);
    }

    #[test]
    fn rm_table_cell_bullet_list() {
        let md = "| A |\n| -- |\n| - one<br>- two |";
        let lines = render_markdown(md, 80, &theme());
        let text = all_text(&lines);
        assert_eq!(text.matches('•').count(), 2, "expected two bullets: {:?}", text);
        assert!(!text.contains("- one"), "marker not stripped: {:?}", text);
    }

    #[test]
    fn rm_table_cell_ordered_list_numbers_increment() {
        let md = "| A |\n| -- |\n| 1. one<br>2. two |";
        let lines = render_markdown(md, 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("1. one"), "{:?}", text);
        assert!(text.contains("2. two"), "{:?}", text);
    }

    #[test]
    fn rm_table_cell_ordered_list_continues_counting() {
        // Repeating `1.` is the idiomatic way to write an ordered list; the
        // renderer must number them 1, 2, 3 rather than repeating "1.".
        let md = "| A |\n| -- |\n| 1. a<br>1. b<br>1. c |";
        let text = all_text(&render_markdown(md, 60, &theme()));
        for expected in ["1. a", "2. b", "3. c"] {
            assert!(text.contains(expected), "missing {:?} in {:?}", expected, text);
        }
    }

    #[test]
    fn rm_table_cell_nested_bullets_indent() {
        let md = "| A |\n| -- |\n| - outer<br>  - inner |";
        let lines = render_markdown(md, 60, &theme());
        let row = |needle: &str| -> String {
            lines.iter().map(|l| line_text(l))
                .find(|t| t.contains(needle))
                .unwrap_or_else(|| panic!("no row with {:?}", needle))
        };
        let outer = row("outer");
        let inner = row("inner");
        let outer_col = outer.find('•').unwrap();
        let inner_col = inner.find('•').unwrap();
        assert!(inner_col > outer_col,
            "nested bullet should be indented further: {:?} vs {:?}", inner, outer);
    }

    #[test]
    fn rm_table_cell_heading() {
        let md = "| A |\n| -- |\n| ## Title |";
        let lines = render_markdown(md, 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("## Title"), "{:?}", text);
        let s = span_with(&lines, "Title");
        assert!(s.style.add_modifier.contains(Modifier::BOLD), "heading should be bold");
    }

    #[test]
    fn rm_table_cell_blockquote() {
        let md = "| A |\n| -- |\n| > quoted |";
        let lines = render_markdown(md, 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("│ quoted"), "expected a quote bar: {:?}", text);
    }

    #[test]
    fn rm_table_cell_rule() {
        let md = "| A |\n| -- |\n| above<br>---<br>below |";
        let lines = render_markdown(md, 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("above") && text.contains("below"), "{:?}", text);
        // The rule row sits between them.
        let idx = |needle: &str| lines.iter().position(|l| line_text(l).contains(needle)).unwrap();
        assert!(idx("above") < idx("below"));
        let rule_row = lines.iter().find(|l| {
            let t = line_text(l);
            t.contains('│') && t.contains("───") && !t.contains("above")
        });
        assert!(rule_row.is_some(), "no rule row rendered: {:?}", text);
    }

    #[test]
    fn rm_table_cell_pre_formatted() {
        let md = "| A |\n| -- |\n| <pre>  x = 1<br>  y = 2</pre> |";
        let lines = render_markdown(md, 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("  x = 1"), "indentation must survive: {:?}", text);
        assert!(text.contains("  y = 2"), "{:?}", text);
    }

    #[test]
    fn rm_table_cell_fence_pre_formatted() {
        let md = "| A |\n| -- |\n| ```<br>  x = 1<br>``` |";
        let lines = render_markdown(md, 80, &theme());
        let text = all_text(&lines);
        assert!(text.contains("  x = 1"), "indentation must survive: {:?}", text);
        assert!(!text.contains("```"), "fence markers must be dropped: {:?}", text);
    }

    #[test]
    fn rm_table_cell_styles_survive_block_markers() {
        // Stripping the `- ` marker must not lose the bold styling after it.
        let md = "| A |\n| -- |\n| - **bold** item |";
        let lines = render_markdown(md, 80, &theme());
        let s = span_with(&lines, "bold");
        assert!(s.style.add_modifier.contains(Modifier::BOLD), "got {:?}", s.style);
        assert!(!s.content.contains("**"));
    }

    #[test]
    fn rm_table_cell_multiline_row_height_matches() {
        // A cell with three lines makes its row three rows tall, and the
        // vertical borders must be drawn on every one of them.
        let md = "| A | B |\n| -- | -- |\n| one<br>two<br>three | x |";
        let lines = render_markdown(md, 80, &theme());
        let content_rows = lines.iter()
            .map(|l| line_text(l))
            .filter(|t| t.contains('│') && (t.contains("one") || t.contains("two") || t.contains("three") || t.contains(" x ")))
            .count();
        assert_eq!(content_rows, 3, "row should be 3 lines tall");
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
