pub mod overlay;
pub mod project_detail;
pub mod project_list;
pub mod todo_list;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use std::time::SystemTime;
use ratatui::style::{Color, Modifier, Style};

/// Returns a [`Style`] for a reminder date string (`YYYY-MM-DD`) based on
/// how many days remain until (or since) that date.  Uses a coloured
/// background with white bold text for active todos, or dark-gray fg with
/// no background for done/canceled ones.
///
/// | Days until date  | Background | Meaning             |
/// |------------------|------------|---------------------|
/// | ≤ 1 (past/today) | Red        | Overdue / due today |
/// | 2–7              | Yellow     | This week           |
/// | 8+               | Green      | Longer              |
pub fn reminder_date_style(date_str: &str, is_done_or_canceled: bool) -> Style {
    if is_done_or_canceled {
        return Style::default().fg(Color::DarkGray);
    }
    let trimmed = &date_str[..date_str.len().min(10)];
    let offset_minutes = crate::tz::get_tz_offset_minutes();
    let today = (DateTime::<Utc>::from(SystemTime::now()) + Duration::minutes(offset_minutes)).date_naive();
    match NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        Ok(date) => {
            let days = (date - today).num_days();
            let bg = if days <= 1 {
                Color::Red      // overdue or due today/tomorrow
            } else if days <= 7 {
                Color::Yellow   // due this week
            } else {
                Color::Green    // further out
            };
            Style::default().bg(bg).fg(Color::White).add_modifier(Modifier::BOLD)
        }
        Err(_) => Style::default().fg(Color::DarkGray),
    }
}

/// Returns how many terminal rows a single-line hint string needs when
/// ratatui word-wraps it inside `width` columns.  Uses ceiling division
/// as a conservative upper bound and caps at 3 so the hint bar can never
/// crowd out the main content on very narrow terminals.
pub fn hint_height(text: &str, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let len = text.chars().count();
    let w = width as usize;
    (len.div_ceil(w).max(1) as u16).min(3)
}

// ─── List pagination helper (shared by multiple list views) ──────────────────

/// Compute the window of item indexes that fully fits into `area_height`
/// rows, keeping `selected` (if any) visible.
///
/// ratatui's `List` scrolls by whole items: when a multi-row item would
/// overflow the bottom it is dropped entirely, leaving a gap, and footer
/// rows appended after the items (e.g. the "N archived" hint) can be
/// pushed past the bottom edge.  Each view therefore slices its items to
/// the window returned here, renders the `List` widget into exactly
/// `rows_used` rows, and paints any leftover rows itself (peek rows).
///
/// Returns `(start, end, rows_used)` where `end` is exclusive and
/// `end - start >= 1` (on very short areas the first item is force-fit
/// and will simply be truncated).  When a selection is supplied it is
/// guaranteed to lie inside the window.
pub fn list_window(
    item_heights: &[usize],
    area_height: usize,
    selected: Option<usize>,
) -> (usize, usize, usize) {
    let n = item_heights.len();
    if n == 0 || area_height == 0 {
        return (0, 0, 0);
    }

    // Anchor the window on the selected item so it is always on screen.
    let mut start = selected.map(|s| s.min(n - 1)).unwrap_or(0);
    let anchor_h = item_heights[start];

    // Walk backwards from the anchor, admitting items until the anchor
    // item no longer fits.
    let mut rows = 0;
    while start > 0 {
        let h = item_heights[start - 1];
        if rows + h + anchor_h <= area_height {
            rows += h;
            start -= 1;
        } else {
            break;
        }
    }

    // Admit following items while they fit fully.
    let mut end = start;
    let mut rows_used = 0;
    while end < n {
        let h = item_heights[end];
        if rows_used + h <= area_height {
            rows_used += h;
            end += 1;
        } else {
            break;
        }
    }

    // Edge case: even the anchor item doesn't fit — force-fit it so it is
    // not lost entirely.
    if rows_used == 0 {
        rows_used = item_heights[start].min(area_height);
        end = (start + 1).min(n);
    }

    (start, end, rows_used)
}

/// Like [`list_window`], but with rows **reserved** for trailing footer
/// items (e.g. the "[N archived — A to show]" hint).
///
/// `core_count` is the number of leading items in `item_heights` that make
/// up the scrollable content; the remaining `item_heights.len() -
/// core_count` items are footers.  The core window is computed against
/// `area_height - total footer height` so the footers can never be pushed
/// off the bottom.
///
/// The returned [`FooterPlan`] describes how to render: the `start..end`
/// window indexes **core items only** (render as one `List` into the first
/// `core_rows` rows of the area), and `kept_footers` footer items starting
/// at index `core_count` (render into a strip covering the last
/// `footer_rows` rows of the area).  Footers are dropped from the end of
/// the footer block only when the area is too short to show them alongside
/// any core content.
pub struct FooterPlan {
    /// First core item in the window (index into the full item list).
    pub start: usize,
    /// One past the last core item in the window (never beyond `core_count`).
    pub end: usize,
    /// Number of footer items (from index `core_count` on) to render.
    pub kept_footers: usize,
    /// Rows the core `List` occupies.
    pub core_rows: usize,
    /// Rows the footer strip occupies (all kept footer items fit fully).
    pub footer_rows: usize,
}

pub fn list_window_reserved(
    item_heights: &[usize],
    area_height: usize,
    selected: Option<usize>,
    core_count: usize,
) -> FooterPlan {
    let n = item_heights.len();
    let core_count = core_count.min(n);
    let n_footers = n - core_count;

    // Shrink the available area by the height of the kept footers.
    let mut kept_footers = n_footers;
    let mut footer_rows: usize = item_heights[core_count..].iter().sum();
    while kept_footers > 0 && footer_rows >= area_height {
        // No room for both core content and the footers: core wins.
        kept_footers -= 1;
        footer_rows -= item_heights[core_count + kept_footers];
    }
    if kept_footers == 0 {
        let (start, end, core_rows) = list_window(item_heights, area_height, selected);
        return FooterPlan { start, end, kept_footers: 0, core_rows, footer_rows: 0 };
    }

    let budget = area_height - footer_rows; // >= 1 by the loop above
    let (start, end, core_rows) =
        list_window(&item_heights[..core_count], budget, selected);
    FooterPlan { start, end, kept_footers, core_rows, footer_rows }
}

// ─── Text wrapping helpers (shared by multiple views) ────────────────────────

/// Hard-wrap `text` at `width` characters per line.
pub fn char_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    for line in text.lines() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            result.push(String::new());
        } else {
            let mut i = 0;
            while i < chars.len() {
                let end = (i + width).min(chars.len());
                result.push(chars[i..end].iter().collect());
                i = end;
            }
        }
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

/// Tokenises a single source line into `(preceding_space_count, word)` pairs,
/// preserving runs of multiple spaces between words so that callers can
/// reconstruct them faithfully.
///
/// Example: `"foo  bar"` → `(0, "foo")`, `(2, "bar")`.
fn line_tokens(line: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut s = line;
    std::iter::from_fn(move || {
        if s.is_empty() {
            return None;
        }
        // ASCII space is always a single byte, so byte count == char count here.
        let n_spaces = s.bytes().take_while(|&b| b == b' ').count();
        s = &s[n_spaces..];
        let n_bytes = s.find(' ').unwrap_or(s.len());
        let word = &s[..n_bytes];
        s = &s[n_bytes..];
        Some((n_spaces, word))
    })
}

/// Word-wrap `text` at `width` columns; falls back to [`char_wrap`] for overlong words.
///
/// Multiple consecutive spaces between words are preserved on the same output
/// line.  Spaces at a wrap boundary (i.e. the spaces that would have started
/// the continuation line) are dropped, which is standard word-wrap behaviour.
pub fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut result = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            result.push(String::new());
            continue;
        }
        let mut current = String::new();
        let mut cur_len = 0usize;

        for (space_len, word) in line_tokens(line) {
            let wlen = word.chars().count();
            if current.is_empty() {
                // Start of an output line (first line or after a wrap).
                // Drop preceding spaces — they are either leading indentation
                // or spaces at the wrap boundary, both normalised away.
                if wlen > width {
                    let mut chunks = char_wrap(word, width).into_iter().peekable();
                    while let Some(chunk) = chunks.next() {
                        let clen = chunk.chars().count();
                        if chunks.peek().is_some() || clen == width {
                            result.push(chunk);
                        } else {
                            current = chunk;
                            cur_len = clen;
                        }
                    }
                } else {
                    current = word.to_string();
                    cur_len = wlen;
                }
            } else if cur_len + space_len + wlen <= width {
                // Word fits on the current line; preserve the exact space run.
                for _ in 0..space_len {
                    current.push(' ');
                }
                current.push_str(word);
                cur_len += space_len + wlen;
            } else {
                // Flush current line; start a new one with just the word
                // (spaces at the wrap boundary are dropped).
                result.push(std::mem::take(&mut current));
                cur_len = 0;
                if wlen > width {
                    let mut chunks = char_wrap(word, width).into_iter().peekable();
                    while let Some(chunk) = chunks.next() {
                        let clen = chunk.chars().count();
                        if chunks.peek().is_some() || clen == width {
                            result.push(chunk);
                        } else {
                            current = chunk;
                            cur_len = clen;
                        }
                    }
                } else {
                    current = word.to_string();
                    cur_len = wlen;
                }
            }
        }
        if !current.is_empty() {
            result.push(current);
        }
    }
    if result.is_empty() {
        result.push(String::new());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{char_wrap, hint_height, list_window, list_window_reserved, word_wrap};

    // ── word_wrap ─────────────────────────────────────────────────────────────

    #[test]
    fn preserves_double_space_between_words() {
        // Core regression: "foo  bar" must not collapse to "foo bar".
        assert_eq!(word_wrap("foo  bar", 20), vec!["foo  bar"]);
    }

    #[test]
    fn preserves_multiple_spaces_between_words() {
        assert_eq!(word_wrap("a  b   c", 20), vec!["a  b   c"]);
    }

    #[test]
    fn drops_double_space_at_wrap_boundary() {
        // The two spaces between "foo" and "bar" fall on the wrap boundary;
        // the continuation line starts with "bar", no leading space.
        assert_eq!(word_wrap("foo  bar", 4), vec!["foo", "bar"]);
    }

    #[test]
    fn single_space_basic_wrap() {
        assert_eq!(word_wrap("hello world", 5), vec!["hello", "world"]);
    }

    #[test]
    fn fits_on_one_line() {
        assert_eq!(word_wrap("hello world", 20), vec!["hello world"]);
    }

    #[test]
    fn empty_text() {
        assert_eq!(word_wrap("", 20), vec![""]);
    }

    #[test]
    fn overlong_word_char_wrapped() {
        assert_eq!(word_wrap("abcdefgh", 4), vec!["abcd", "efgh"]);
    }

    #[test]
    fn multiline_input() {
        let input = "foo  bar\nbaz  qux";
        assert_eq!(word_wrap(input, 20), vec!["foo  bar", "baz  qux"]);
    }

    #[test]
    fn blank_line_preserved() {
        let input = "foo\n\nbar";
        assert_eq!(word_wrap(input, 20), vec!["foo", "", "bar"]);
    }

    // ── char_wrap ─────────────────────────────────────────────────────────────

    #[test]
    fn char_wrap_basic() {
        assert_eq!(char_wrap("abcdef", 3), vec!["abc", "def"]);
    }

    #[test]
    fn char_wrap_empty_string() {
        assert_eq!(char_wrap("", 5), vec![""]);
    }

    #[test]
    fn char_wrap_exact_width_no_split() {
        assert_eq!(char_wrap("abc", 3), vec!["abc"]);
    }

    #[test]
    fn char_wrap_width_one_splits_every_char() {
        assert_eq!(char_wrap("abc", 1), vec!["a", "b", "c"]);
    }

    #[test]
    fn char_wrap_multiline_input_each_line_independently() {
        assert_eq!(char_wrap("ab\ncd", 5), vec!["ab", "cd"]);
    }

    #[test]
    fn char_wrap_respects_char_boundaries_for_multibyte() {
        // 'é' is 1 char (2 UTF-8 bytes); width is in chars, not bytes.
        assert_eq!(char_wrap("h\u{00e9}llo", 3), vec!["h\u{00e9}l", "lo"]);
    }

    // ── hint_height ───────────────────────────────────────────────────────────────────

    #[test]
    fn hint_height_zero_width_always_returns_one() {
        assert_eq!(hint_height("hello world", 0), 1);
        assert_eq!(hint_height("", 0), 1);
    }

    #[test]
    fn hint_height_empty_text_is_one_row() {
        assert_eq!(hint_height("", 80), 1);
    }

    #[test]
    fn hint_height_text_fits_in_one_row() {
        assert_eq!(hint_height("hello", 80), 1);
    }

    #[test]
    fn hint_height_text_exactly_fills_width_is_one_row() {
        let text = "a".repeat(80);
        assert_eq!(hint_height(&text, 80), 1);
    }

    #[test]
    fn hint_height_two_rows_when_text_spans_two_widths() {
        let text = "a".repeat(100);
        assert_eq!(hint_height(&text, 50), 2);
    }

    #[test]
    fn hint_height_is_capped_at_three() {
        // 400 chars at width 50 would be 8 rows; capped to 3.
        let text = "a".repeat(400);
        assert_eq!(hint_height(&text, 50), 3);
    }

    // ── additional word_wrap edge cases ──────────────────────────────────────────

    #[test]
    fn word_wrap_words_fill_exact_width_per_line() {
        // "ab cd ef" at width 5: "ab cd" (5) fits, "ef" wraps.
        assert_eq!(word_wrap("ab cd ef", 5), vec!["ab cd", "ef"]);
    }

    #[test]
    fn word_wrap_three_words_wrap_at_boundary() {
        // "one two three" at width 7: "one two" (7) fits, "three" wraps.
        assert_eq!(word_wrap("one two three", 7), vec!["one two", "three"]);
    }

    #[test]
    fn word_wrap_single_word_never_wraps() {
        assert_eq!(word_wrap("hello", 10), vec!["hello"]);
    }

    // ── list_window ──────────────────────────────────────────────────────────

    #[test]
    fn list_window_all_items_fit() {
        let heights = vec![1, 1, 1, 1];
        assert_eq!(list_window(&heights, 10, None), (0, 4, 4));
        assert_eq!(list_window(&heights, 10, Some(2)), (0, 4, 4));
    }

    #[test]
    fn list_window_drops_item_that_does_not_fit() {
        // Heights 1,1,5: with an area of 7 the 5-row item fits (1+1+5=7),
        // but with an area of 6 it doesn't — it should be dropped entirely
        // rather than leaving a gap under a partially rendered item.
        let heights = vec![1, 1, 5];
        assert_eq!(list_window(&heights, 7, None), (0, 3, 7));
        assert_eq!(list_window(&heights, 6, None), (0, 2, 2));
    }

    #[test]
    fn list_window_keeps_selected_visible() {
        // Five single-row items in an area of 3, cursor on item 3:
        // window must contain the selection.
        let heights = vec![1, 1, 1, 1, 1];
        assert_eq!(list_window(&heights, 3, Some(3)), (1, 4, 3));
        assert_eq!(list_window(&heights, 3, Some(0)), (0, 3, 3));
        assert_eq!(list_window(&heights, 3, Some(4)), (2, 5, 3));
    }

    #[test]
    fn list_window_anchor_multi_row_item_not_split() {
        // A 3-row selected item in an area of 4: it must be the window's
        // anchor and fully visible; items above are admitted only while
        // everything still fits.
        let heights = vec![1, 1, 3, 1, 1];
        let (start, end, used) = list_window(&heights, 4, Some(2));
        assert!(start <= 2 && 3 <= end);
        assert!((start..end).contains(&2));
        assert_eq!(used, 4);
    }

    #[test]
    fn list_window_tiny_area_force_fits_anchor() {
        // Area of 2 with a 3-row selected item: the item is force-fit and
        // will be truncated by the area height.
        let heights = vec![3, 1];
        assert_eq!(list_window(&heights, 2, Some(0)), (0, 1, 2));
    }

    #[test]
    fn list_window_empty_and_degenerate_inputs() {
        assert_eq!(list_window(&[], 5, None), (0, 0, 0));
        assert_eq!(list_window(&[1, 1], 0, None), (0, 0, 0));
        // Selection out of bounds is clamped to the last item.
        assert_eq!(list_window(&[1, 1], 5, Some(99)), (0, 2, 2));
    }

    #[test]
    fn list_window_footer_row_stays_visible() {
        // Regression: the "N archived" footer row (last item) must remain
        // visible when the list fills the area exactly.
        let heights = vec![1, 1, 1, 1]; // 3 todos + 1 footer row
        let (start, end, _used) = list_window(&heights, 4, Some(0));
        assert_eq!(end, 4); // footer included
        assert_eq!(start, 0);
    }

    // ── list_window_reserved ─────────────────────────────────────────────

    #[test]
    fn reserved_footer_fully_visible_at_every_height() {
        // Regression: with a footer item (height 1) and 6 single-row core
        // items, the footer must be kept for ANY area height, including
        // the awkward off-by-one sizes where plain `list_window` filled
        // the area to the bottom and sliced the footer away.
        let heights = vec![1, 1, 1, 1, 1, 1, 1]; // 6 core + 1 footer
        for area in 2..=12 {
            let plan = list_window_reserved(&heights, area, Some(0), 6);
            assert_eq!(plan.kept_footers, 1, "footer dropped at area {area}");
            assert_eq!(plan.footer_rows, 1);
            assert!(plan.core_rows + plan.footer_rows <= area);
            assert!(plan.end <= 6);
        }
    }

    #[test]
    fn reserved_footer_no_footers_matches_list_window() {
        let heights = vec![1, 1, 5];
        for area in [1usize, 2, 5, 6, 7, 10] {
            for sel in [None, Some(0), Some(2)] {
                let plan = list_window_reserved(&heights, area, sel, 3);
                let (start, end, used) = list_window(&heights, area, sel);
                assert_eq!((plan.start, plan.end, plan.core_rows), (start, end, used));
                assert_eq!(plan.kept_footers, 0, "area {area}, sel {sel:?}");
            }
        }
    }

    #[test]
    fn reserved_footer_selection_stays_visible() {
        // 8 core items + 1 footer row in an area of 5: window anchored on
        // the selection, footer always present.
        let heights = vec![1; 9];
        let plan = list_window_reserved(&heights, 5, Some(7), 8);
        assert_eq!(plan.kept_footers, 1);
        assert_eq!(plan.core_rows + plan.footer_rows, 5);
        assert!((plan.start..plan.end).contains(&7));
        assert!(plan.end <= 8);
    }

    #[test]
    fn reserved_footer_tiny_area_drops_footer_not_content() {
        // Area of 1 with a footer: the footer cannot coexist with core
        // content, so it is dropped and the plain window is used.
        let heights = vec![1, 1, 1, 1]; // 3 core + 1 footer
        let plan = list_window_reserved(&heights, 1, None, 3);
        assert_eq!(plan.kept_footers, 0);
        assert_eq!((plan.start, plan.end, plan.core_rows), (0, 1, 1));
    }

    #[test]
    fn reserved_footer_tiny_area_keeps_selection() {
        // Area of 2 with one footer row and the selection on core item 5:
        // the footer still fits and the selection is visible.
        let heights = vec![1; 7]; // 6 core + 1 footer
        let plan = list_window_reserved(&heights, 2, Some(5), 6);
        assert_eq!(plan.kept_footers, 1);
        assert_eq!(plan.footer_rows, 1);
        assert_eq!(plan.core_rows, 1);
        assert!((plan.start..plan.end).contains(&5));
    }

    #[test]
    fn reserved_footer_area_too_small_for_anything() {
        // Degenerate: area 0.
        let plan = list_window_reserved(&[1, 1, 1], 0, None, 2);
        assert_eq!((plan.start, plan.end, plan.core_rows), (0, 0, 0));
        assert_eq!(plan.kept_footers, 0);
    }
}
