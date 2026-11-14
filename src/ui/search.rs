//! Search-match highlighting over rendered ratatui lines — shared by `ptm`
//! and `md`.
//!
//! Both binaries highlight search matches in *rendered* output: the markdown
//! renderer has already word-wrapped the text and split it into styled spans.
//! Matching therefore has to happen here, on the rendered `Line`s, rather than
//! on the source document.
//!
//! ## Why matching is per *line*, not per span
//!
//! `markdown.rs` emits one span per word (and separate spans for markup such
//! as `**bold**`, list markers and table cells), so a query containing a space
//! never fits inside a single span: `/We the people` highlighted nothing at
//! all, and the highlight vanished the moment a space was typed.  Matching the
//! concatenated text of a line and then mapping the char ranges back onto the
//! individual spans fixes that while keeping every span's base style.
//!
//! A match cannot cross a *rendered line* boundary (a word-wrap point or a
//! block break): the lines are searched independently.  That keeps the
//! `(line, occ)` coordinates that `n`/`N` navigation is built on exact.
//!
//! ## Why this lives in its own file
//!
//! `ptm`'s Description tab and update viewer, and `md`'s viewer, all need it.
//! They used to carry near-identical copies (`split_span` +
//! `apply_search_highlight` + `style_current_match` in `views/project_detail.rs`,
//! `compute_search_matches` + `split_span_hits` + `highlight_lines_with_current`
//! in `bin/md/main.rs`) which had already drifted: the `md` copy incremented
//! its occurrence counter only for the *current* match, so with several
//! matches on one line `n` moved the pointer but no match was ever painted
//! yellow.  `ui/text.rs` stays dependency-light (`unicode-width` only), so the
//! highlighter needs its own module instead of living there.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::ui::text::find_matches;

// ─── Types ───────────────────────────────────────────────────────────────────

/// One search match, in coordinates of the *rendered* (wrapped) output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MatchLoc {
    /// Line index into the rendered line vector.
    pub line: usize,
    /// Ordinal of this occurrence within its line (0-based), in match order
    /// across all spans of that line.
    pub occ: usize,
}

/// A highlighted buffer: the restyled lines plus every match position, in
/// document order.
pub struct Highlighted {
    pub lines: Vec<Line<'static>>,
    /// Every match, ordered by line and occurrence.  Index it with the
    /// current-match pointer (`pd_match_idx` / `match_idx`) for `n`/`N`.
    pub marks: Vec<MatchLoc>,
}

// ─── Public API ──────────────────────────────────────────────────────────────

/// Highlight every case-insensitive occurrence of `lower_query` (a char slice,
/// pre-lowercased) in `lines`.
///
/// The match at flat index `current_idx` (into the returned [`Highlighted::marks`])
/// gets `current_style`; every other match gets `other_style`.  The index is
/// clamped to the last match, so a stale pointer left over from a previous
/// frame cannot un-highlight everything.  Non-match text keeps the style of the
/// span it came from.
///
/// An empty query (or no matches) returns the lines untouched and no marks.
pub fn highlight(
    lines: Vec<Line<'static>>,
    lower_query: &[char],
    current_idx: Option<usize>,
    current_style: Style,
    other_style: Style,
) -> Highlighted {
    if lower_query.is_empty() {
        return Highlighted { lines, marks: Vec::new() };
    }

    // Pass 1: locate the matches of every line, in document order.  Doing this
    // before restyling is what lets `current_idx` be clamped against the real
    // number of matches.
    let per_line: Vec<Vec<(usize, usize)>> = lines
        .iter()
        .map(|line| line_match_ranges(line, lower_query))
        .collect();
    let total: usize = per_line.iter().map(Vec::len).sum();
    if total == 0 {
        return Highlighted { lines, marks: Vec::new() };
    }
    let current = current_idx.map(|i| i.min(total - 1));

    let mut marks = Vec::with_capacity(total);
    for (line_idx, ranges) in per_line.iter().enumerate() {
        for occ in 0..ranges.len() {
            marks.push(MatchLoc { line: line_idx, occ });
        }
    }

    // Pass 2: split each line's spans at the match boundaries.
    let mut lines = lines;
    let mut base_occ = 0usize;
    for (line_idx, line) in lines.iter_mut().enumerate() {
        let ranges = &per_line[line_idx];
        if !ranges.is_empty() {
            let spans = std::mem::take(&mut line.spans);
            line.spans = restyle(spans, ranges, base_occ, current, current_style, other_style);
        }
        base_occ += ranges.len();
    }

    Highlighted { lines, marks }
}

/// Highlight every match in a single line with one style.
///
/// For lists that highlight but do not navigate (the Todos tab, the Updates
/// list preview).  The line's own style/alignment is preserved.
pub fn highlight_line(line: Line<'static>, lower_query: &[char], style: Style) -> Line<'static> {
    if lower_query.is_empty() {
        return line;
    }
    let ranges = line_match_ranges(&line, lower_query);
    if ranges.is_empty() {
        return line;
    }
    let mut line = line;
    let spans = std::mem::take(&mut line.spans);
    line.spans = restyle(spans, &ranges, 0, None, style, style);
    line
}

// ─── Internals ───────────────────────────────────────────────────────────────

/// Char ranges `(start, end)` of every match in `line`, in coordinates of the
/// line's concatenated span text.
///
/// Concatenating first is the whole point: the markdown renderer splits a
/// paragraph into one span per word, so searching span by span can never match
/// a phrase.
fn line_match_ranges(line: &Line<'static>, lower_query: &[char]) -> Vec<(usize, usize)> {
    let mut text = String::new();
    for span in &line.spans {
        text.push_str(&span.content);
    }
    find_matches(&text, lower_query)
}

/// Split `spans` so that every char range in `ranges` becomes its own span,
/// styled with the current/other match style.  `base_occ` is the flat match
/// index of this line's first match; `current` is the flat current-match index.
fn restyle(
    spans: Vec<Span<'static>>,
    ranges: &[(usize, usize)],
    base_occ: usize,
    current: Option<usize>,
    current_style: Style,
    other_style: Style,
) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len() + ranges.len() * 2);
    let mut span_start = 0usize; // char offset of the current span in the line
    for span in spans {
        let chars: Vec<char> = span.content.chars().collect();
        let span_end = span_start + chars.len();
        let base = span.style;

        // `ranges` is sorted and non-overlapping (find_matches guarantees it),
        // so a single forward pass per span is enough.
        let mut cursor = span_start;
        for (j, &(m_start, m_end)) in ranges.iter().enumerate() {
            // Clip the match to this span; skip ranges with no overlap.
            let start = m_start.max(span_start);
            let end = m_end.min(span_end);
            if start >= end {
                continue;
            }
            if start > cursor {
                out.push(Span::styled(slice(&chars, cursor, start, span_start), base));
            }
            let style = if current == Some(base_occ + j) { current_style } else { other_style };
            out.push(Span::styled(slice(&chars, start, end, span_start), style));
            cursor = end;
        }
        if cursor < span_end {
            out.push(Span::styled(slice(&chars, cursor, span_end, span_start), base));
        }
        span_start = span_end;
    }
    out
}

/// `chars[from - offset .. to - offset]` as an owned string.
fn slice(chars: &[char], from: usize, to: usize, offset: usize) -> String {
    chars[from - offset..to - offset].iter().collect()
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{highlight, highlight_line, MatchLoc};
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};

    fn q(query: &str) -> Vec<char> {
        query.to_lowercase().chars().collect()
    }

    fn cur_style() -> Style {
        Style::default().bg(Color::Yellow)
    }

    fn other_style() -> Style {
        Style::default().bg(Color::DarkGray)
    }

    /// The text of every span carrying `style`'s background, in order.
    fn styled(spans: &[Span<'static>], style: Style) -> Vec<String> {
        spans
            .iter()
            .filter(|s| s.style.bg == style.bg)
            .map(|s| s.content.to_string())
            .collect()
    }

    fn line_of(parts: &[&'static str]) -> Line<'static> {
        Line::from(parts.iter().map(|p| Span::raw(*p)).collect::<Vec<_>>())
    }

    #[test]
    fn every_match_on_one_line_is_found_in_order() {
        let lines = vec![line_of(&["alpha beta alpha alpha"])];
        let hl = highlight(lines, &q("alpha"), Some(0), cur_style(), other_style());
        assert_eq!(
            hl.marks,
            vec![
                MatchLoc { line: 0, occ: 0 },
                MatchLoc { line: 0, occ: 1 },
                MatchLoc { line: 0, occ: 2 },
            ]
        );
    }

    #[test]
    fn the_current_match_pointer_selects_which_occurrence_is_yellow() {
        // Regression: with several matches on one line, `n` used to move the
        // pointer but leave every match gray (the old `md` copy only counted
        // occurrences while looking for the current one).
        let lines = vec![line_of(&["alpha beta alpha alpha"])];
        for idx in 0..3 {
            let hl = highlight(lines.clone(), &q("alpha"), Some(idx), cur_style(), other_style());
            assert_eq!(
                styled(&hl.lines[0].spans, cur_style()),
                vec!["alpha"],
                "exactly one match must be yellow for idx {idx}"
            );
            assert_eq!(styled(&hl.lines[0].spans, other_style()).len(), 2);
            // The yellow one must be the idx-th occurrence, left to right.
            let yellow_pos = hl.lines[0]
                .spans
                .iter()
                .position(|s| s.style.bg == cur_style().bg)
                .expect("a current match");
            let before: String = hl.lines[0].spans[..yellow_pos]
                .iter()
                .map(|s| s.content.as_ref())
                .collect();
            assert_eq!(before.matches("alpha").count(), idx, "wrong occurrence at idx {idx}");
        }
    }

    #[test]
    fn a_phrase_matches_across_the_per_word_spans_markdown_emits() {
        // `render_markdown` emits one span per word; a phrase therefore never
        // lives inside a single span.  This is the `/We the people` case.
        let lines = vec![line_of(&["We", " ", "the", " ", "people", " of the United States"])];
        let hl = highlight(lines, &q("We the people"), Some(0), cur_style(), other_style());
        assert_eq!(hl.marks, vec![MatchLoc { line: 0, occ: 0 }]);
        let yellow: String = hl.lines[0]
            .spans
            .iter()
            .filter(|s| s.style.bg == cur_style().bg)
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(yellow, "We the people");
    }

    #[test]
    fn a_phrase_match_keeps_every_character_and_the_base_styles() {
        let bold = Style::default().add_modifier(ratatui::style::Modifier::BOLD);
        let lines = vec![Line::from(vec![
            Span::styled("We the ", Style::default()),
            Span::styled("people", bold),
            Span::raw(" said"),
        ])];
        let hl = highlight(lines, &q("the people"), Some(0), cur_style(), other_style());
        let joined: String = hl.lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "We the people said");
        // Non-match text keeps the style it arrived with.
        let bold_piece = hl.lines[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == " said")
            .expect("the tail span");
        assert_eq!(bold_piece.style, Style::default());
    }

    #[test]
    fn matching_is_case_insensitive_and_does_not_drift_on_folding() {
        let lines = vec![line_of(&["İstanbul"])];
        let hl = highlight(lines.clone(), &q("İ"), Some(0), cur_style(), other_style());
        assert_eq!(styled(&hl.lines[0].spans, cur_style()), vec!["İ"]);
        let hl = highlight(lines.clone(), &q("i"), Some(0), cur_style(), other_style());
        assert_eq!(styled(&hl.lines[0].spans, cur_style()), vec!["İ"]);
        let hl = highlight(lines, &q("stan"), Some(0), cur_style(), other_style());
        assert_eq!(styled(&hl.lines[0].spans, cur_style()), vec!["stan"]);
    }

    #[test]
    fn marks_run_in_document_order_across_lines() {
        let lines = vec![
            line_of(&["alpha alpha"]),
            line_of(&["nothing"]),
            line_of(&["alpha"]),
        ];
        let hl = highlight(lines, &q("alpha"), Some(2), cur_style(), other_style());
        assert_eq!(
            hl.marks,
            vec![
                MatchLoc { line: 0, occ: 0 },
                MatchLoc { line: 0, occ: 1 },
                MatchLoc { line: 2, occ: 0 },
            ]
        );
        // The current pointer (flat index 2) is the match on line 2.
        assert!(styled(&hl.lines[0].spans, cur_style()).is_empty());
        assert_eq!(styled(&hl.lines[2].spans, cur_style()), vec!["alpha"]);
    }

    #[test]
    fn a_stale_current_index_clamps_to_the_last_match() {
        let lines = vec![line_of(&["alpha alpha"])];
        let hl = highlight(lines, &q("alpha"), Some(99), cur_style(), other_style());
        let yellow_pos = hl.lines[0]
            .spans
            .iter()
            .position(|s| s.style.bg == cur_style().bg)
            .expect("a current match");
        let before: String = hl.lines[0].spans[..yellow_pos]
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(before.matches("alpha").count(), 1, "must clamp to the last match");
    }

    #[test]
    fn an_empty_query_or_no_matches_leaves_the_lines_untouched() {
        let lines = vec![line_of(&["alpha"])];
        let hl = highlight(lines.clone(), &[], Some(0), cur_style(), other_style());
        assert!(hl.marks.is_empty());
        assert_eq!(hl.lines[0].spans.len(), 1);

        let hl = highlight(lines.clone(), &q("zzz"), Some(0), cur_style(), other_style());
        assert!(hl.marks.is_empty());
        assert_eq!(hl.lines[0].spans.len(), 1);
    }

    #[test]
    fn line_style_and_alignment_survive_highlighting() {
        let style = Style::default().fg(Color::Red);
        let line = Line::from(vec![Span::raw("alpha")]).style(style);
        let hl = highlight(vec![line], &q("alpha"), Some(0), cur_style(), other_style());
        assert_eq!(hl.lines[0].style, style);
    }

    #[test]
    fn highlight_line_styles_every_match_and_drops_nothing() {
        let line = Line::from(vec![Span::raw("  "), Span::raw("alpha beta alpha")]);
        let out = highlight_line(line, &q("alpha"), cur_style());
        assert_eq!(styled(&out.spans, cur_style()), vec!["alpha", "alpha"]);
        let joined: String = out.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "  alpha beta alpha");
    }

    #[test]
    fn a_wide_character_match_is_not_split() {
        let lines = vec![line_of(&["日本語テキスト"])];
        let hl = highlight(lines, &q("語"), Some(0), cur_style(), other_style());
        assert_eq!(styled(&hl.lines[0].spans, cur_style()), vec!["語"]);
        let joined: String = hl.lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(joined, "日本語テキスト");
    }
}
