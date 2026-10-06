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
    use super::{char_wrap, hint_height, word_wrap};

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
}
