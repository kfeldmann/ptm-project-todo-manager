//! Display-width-aware text helpers, shared by every renderer.
//!
//! Terminal layout is measured in **columns**, not characters or bytes.  A CJK
//! ideograph or an emoji occupies two columns; a combining mark occupies none;
//! a byte is not a unit of display at all.  Every wrap, truncation and padding
//! decision in the UI therefore goes through the helpers here instead of
//! `str::chars().count()` / `str::len()` (finding #16), and the case-insensitive
//! search matcher lives here too so the two copies of it cannot drift apart
//! (finding #17).
//!
//! The module is dependency-light on purpose: `unicode-width` and nothing else.
//! It is path-included by the `md` binary (`src/bin/md/ui.rs`) exactly like
//! `theme.rs` and `markdown.rs`, so `ptm` and `md` wrap identically.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// ─── Width primitives ────────────────────────────────────────────────────────

/// Terminal display width of `s`, in columns.
#[inline]
pub fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Terminal display width of a single `char`, in columns.
///
/// Unassigned / control characters have no width of their own; they are
/// counted as zero rather than panicking.
#[inline]
pub fn char_display_width(c: char) -> usize {
    UnicodeWidthChar::width(c).unwrap_or(0)
}

/// Pad `s` on the right with spaces so it occupies exactly `width` columns.
///
/// `format!("{:<width$}")` pads by **characters**, which misaligns any column
/// containing wide text.  A string already at or beyond `width` columns is
/// returned unchanged (it is never truncated here).
pub fn pad_to_width(s: &str, width: usize) -> String {
    let mut out = s.to_string();
    let w = display_width(s);
    for _ in w..width {
        out.push(' ');
    }
    out
}

/// Truncate `s` to `width` columns and **always** end with `…`.
///
/// The ellipsis column is reserved unconditionally, even when `s` already fits:
/// this is for the "there is more text below" marker, where dropping the
/// ellipsis would lose the signal.  Returns an empty string for `width == 0`.
pub fn ellipsize(s: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let budget = width - 1;
    let mut out = String::new();
    let mut used = 0usize;
    for ch in s.chars() {
        let cw = char_display_width(ch);
        if used + cw > budget {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

// ─── Wrapping ────────────────────────────────────────────────────────────────

/// Hard-wrap `text` at `width` columns, preserving every character.
///
/// `width == 0` disables wrapping (there is no meaningful column to break at).
/// Always returns at least one element.
pub fn char_wrap(text: &str, width: usize) -> Vec<String> {
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
        let mut cur_w = 0usize;
        for ch in line.chars() {
            let cw = char_display_width(ch);
            // Break *before* the character that would overflow.  `current` is
            // never empty here except for a character wider than the budget,
            // which must be emitted on a line of its own to make progress.
            if cur_w + cw > width && !current.is_empty() {
                result.push(std::mem::take(&mut current));
                cur_w = 0;
            }
            current.push(ch);
            cur_w += cw;
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

/// Word-wrap `text` at `width` columns; falls back to [`char_wrap`] for
/// overlong words.
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
        let mut cur_w = 0usize;

        for (space_len, word) in line_tokens(line) {
            let wlen = display_width(word);
            if current.is_empty() {
                // Start of an output line (first line or after a wrap).
                // Drop preceding spaces — they are either leading indentation
                // or spaces at the wrap boundary, both normalised away.
                if wlen > width {
                    hard_wrap_word(word, width, &mut result, &mut current, &mut cur_w);
                } else {
                    current = word.to_string();
                    cur_w = wlen;
                }
            } else if cur_w + space_len + wlen <= width {
                // Word fits on the current line; preserve the exact space run.
                for _ in 0..space_len {
                    current.push(' ');
                }
                current.push_str(word);
                cur_w += space_len + wlen;
            } else {
                // Flush current line; start a new one with just the word
                // (spaces at the wrap boundary are dropped).
                result.push(std::mem::take(&mut current));
                cur_w = 0;
                if wlen > width {
                    hard_wrap_word(word, width, &mut result, &mut current, &mut cur_w);
                } else {
                    current = word.to_string();
                    cur_w = wlen;
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

/// Emit all but the last [`char_wrap`] chunk of `word` as complete lines and
/// leave the trailing chunk in `current`, so following words can join it.
fn hard_wrap_word(
    word: &str,
    width: usize,
    result: &mut Vec<String>,
    current: &mut String,
    cur_w: &mut usize,
) {
    let mut chunks = char_wrap(word, width);
    if let Some(last) = chunks.pop() {
        result.extend(chunks);
        *current = last;
        *cur_w = display_width(current);
    }
}

/// Word-wrap one already-split string at `width` **columns**, breaking at the
/// last space inside the budget (the space is dropped) or hard-breaking when
/// the budget holds none.
///
/// This is [`word_wrap`] without the leading-space bookkeeping: the caller has
/// a single string and wants it chopped into rows that each fit `width`
/// columns.  `width == 0` returns the string unwrapped, matching the "there is
/// no column to break at" rule used throughout this module.
pub fn wrap_rows(s: &str, width: usize) -> Vec<String> {
    if width == 0 || s.is_empty() {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut rest = s;
    loop {
        // Collect the characters that fit, then peek one column past the
        // budget so a space sitting exactly at the boundary is preferred over
        // a hard break.
        let mut used = 0usize;
        let mut end = 0usize; // byte offset just past the last fitting char
        let mut overflowed = false;
        for (bi, ch) in rest.char_indices() {
            let cw = char_display_width(ch);
            if used + cw > width {
                overflowed = true;
                break;
            }
            used += cw;
            end = bi + ch.len_utf8();
        }
        if !overflowed {
            out.push(rest.to_string());
            return out;
        }
        // `end` is the byte offset of the first character that does not fit.
        // A single character wider than the whole budget leaves it at 0; take
        // that character anyway so the loop always makes progress.
        if end == 0 {
            end = rest.chars().next().map(char::len_utf8).unwrap_or(0);
        }
        // Look one character past the budget for a space to break on.
        let look_end = rest[end..]
            .chars()
            .next()
            .map(|c| end + c.len_utf8())
            .unwrap_or(end);
        let cut = match rest[..look_end].rfind(' ') {
            Some(i) if i > 0 => i,       // break on the space, dropping it
            _ => end,                    // hard break at the column boundary
        };
        out.push(rest[..cut].to_string());
        // Skip the space the break consumed, if any.
        rest = rest[cut..].trim_start_matches(' ');
        if rest.is_empty() {
            return out;
        }
    }
}

/// Split `s` into `(prefix, rest)` at `width` **columns**, preferring the last
/// ASCII space inside that budget (the space stays on the prefix).
///
/// The caller uses this to wrap a pre-existing slice of text while keeping a
/// running column count: the prefix always fits `width` columns, and `rest` is
/// never the whole of `s` unless `s` is empty — a character wider than the
/// budget is emitted on its own rather than looping forever.
pub fn split_at_width(s: &str, width: usize) -> (&str, &str) {
    if s.is_empty() {
        return ("", "");
    }
    if width == 0 {
        // No budget at all: emit one character so the caller makes progress.
        let end = s.chars().next().map(char::len_utf8).unwrap_or(0);
        return s.split_at(end);
    }
    let mut used = 0usize;
    let mut hard_end = 0usize;
    for (bi, ch) in s.char_indices() {
        let cw = char_display_width(ch);
        if used + cw > width {
            hard_end = bi;
            break;
        }
        used += cw;
        hard_end = bi + ch.len_utf8();
    }
    // A single character wider than the whole budget would leave `hard_end`
    // at 0; take it anyway so the caller cannot loop.
    if hard_end == 0 {
        hard_end = s.chars().next().map(char::len_utf8).unwrap_or(0);
    }
    match s[..hard_end].rfind(' ') {
        Some(p) => s.split_at(p + 1),
        None => s.split_at(hard_end),
    }
}

// ─── Hint bar ────────────────────────────────────────────────────────────────

/// Returns how many terminal rows a single-line hint string needs when
/// ratatui word-wraps it inside `width` columns.
///
/// Counts display columns (a wide character takes two), uses ceiling division
/// as a conservative upper bound, and caps at 3 so the hint bar can never
/// crowd out the main content on very narrow terminals.
pub fn hint_height(text: &str, width: u16) -> u16 {
    if width == 0 {
        return 1;
    }
    let w = width as usize;
    (display_width(text).div_ceil(w).max(1) as u16).min(3)
}

// ─── Case-insensitive matching ───────────────────────────────────────────────

/// Char ranges `(start, end)` — `end` exclusive — of every case-insensitive
/// occurrence of `lower_query` in `text`, in coordinates of **`text`**.
///
/// `lower_query` is pre-lowercased by the caller (`query.to_lowercase()`).
///
/// The original implementation lowercased the text and then used those offsets
/// against the original, which is wrong because case folding is not
/// length-preserving: `'İ'.to_lowercase()` is two chars (`i` + U+0307), so
/// searching `İ` in `İstanbul` highlighted `İs` instead of `İ` (finding #17).
/// Here each lowercased character remembers the index of the original
/// character it came from, so a match always maps back onto whole original
/// characters.
pub fn find_matches(text: &str, lower_query: &[char]) -> Vec<(usize, usize)> {
    if lower_query.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let mut lower: Vec<char> = Vec::new();
    let mut origin: Vec<usize> = Vec::new();
    for (oi, ch) in text.chars().enumerate() {
        for lc in ch.to_lowercase() {
            lower.push(lc);
            origin.push(oi);
        }
    }

    let qlen = lower_query.len();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i + qlen <= lower.len() {
        if lower[i..i + qlen] == *lower_query {
            let start = origin[i];
            let end = origin[i + qlen - 1] + 1;
            out.push((start, end));
            i += qlen;
        } else {
            i += 1;
        }
    }
    out
}

/// Split `text` into `(segment, is_match)` pieces at every case-insensitive
/// occurrence of `lower_query`.
///
/// The concatenation of the segments is always exactly `text`, and the
/// segments flagged `true` are the matched regions of the *original* text.
/// With no matches the whole string comes back as a single non-match piece, so
/// callers never have to special-case an empty result.
pub fn split_matches(text: &str, lower_query: &[char]) -> Vec<(String, bool)> {
    let ranges = find_matches(text, lower_query);
    if ranges.is_empty() {
        return vec![(text.to_string(), false)];
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut pos = 0usize;
    for (start, end) in ranges {
        if start > pos {
            out.push((chars[pos..start].iter().collect(), false));
        }
        out.push((chars[start..end].iter().collect(), true));
        pos = end;
    }
    if pos < chars.len() {
        out.push((chars[pos..].iter().collect(), false));
    }
    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{
        char_wrap, display_width, ellipsize, find_matches, hint_height, pad_to_width,
        split_at_width, split_matches, word_wrap, wrap_rows,
    };

    // ── width primitives ─────────────────────────────────────────────────────

    #[test]
    fn ascii_counts_one_column_per_char() {
        assert_eq!(display_width("hello"), 5);
        assert_eq!(display_width(""), 0);
    }

    #[test]
    fn cjk_and_emoji_count_two_columns() {
        assert_eq!(display_width("日本語"), 6);
        assert_eq!(display_width("🚀"), 2);
        // Mixed: 1 + 2 + 1
        assert_eq!(display_width("a日b"), 4);
    }

    #[test]
    fn combining_marks_count_zero_columns() {
        // "e" + COMBINING ACUTE ACCENT
        assert_eq!(display_width("e\u{301}"), 1);
    }

    // ── pad_to_width ────────────────────────────────────────────────────────

    #[test]
    fn pad_pads_ascii_to_the_column_budget() {
        assert_eq!(pad_to_width("ab", 5), "ab   ");
        assert_eq!(pad_to_width("abcde", 5), "abcde");
    }

    #[test]
    fn pad_accounts_for_wide_characters() {
        // Two CJK chars already occupy four columns.
        assert_eq!(pad_to_width("日本", 5), "日本 ");
        // ... so no padding is added at four.
        assert_eq!(pad_to_width("日本", 4), "日本");
    }

    #[test]
    fn pad_never_truncates() {
        assert_eq!(pad_to_width("abcdef", 3), "abcdef");
    }

    // ── ellipsize ───────────────────────────────────────────────────────────

    #[test]
    fn ellipsize_always_ends_with_an_ellipsis() {
        assert_eq!(ellipsize("abc", 5), "abc…");
        assert_eq!(ellipsize("abcdefgh", 5), "abcd…");
        assert_eq!(ellipsize("", 5), "…");
    }

    #[test]
    fn ellipsize_result_always_fits_the_budget() {
        for text in ["abc", "abcdefghij", "日本語のテキスト", "🚀🚀🚀"] {
            for width in 1..=12usize {
                let out = ellipsize(text, width);
                assert!(
                    display_width(&out) <= width,
                    "{text:?} at {width}: {out:?} is {} columns",
                    display_width(&out)
                );
                assert!(out.ends_with('…'), "{out:?}");
            }
        }
    }

    #[test]
    fn ellipsize_never_splits_a_wide_character() {
        // 10 columns of budget leaves 9 for text: four CJK chars (8 cols) fit,
        // the fifth would overflow and is dropped whole.
        assert_eq!(ellipsize("日本語のテキスト", 10), "日本語の…");
    }

    #[test]
    fn ellipsize_zero_width_is_empty() {
        assert_eq!(ellipsize("abc", 0), "");
    }

    // ── char_wrap ───────────────────────────────────────────────────────────

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
        // 'é' is 1 char, 1 column; width is in columns.
        assert_eq!(char_wrap("h\u{00e9}llo", 3), vec!["h\u{00e9}l", "lo"]);
    }

    /// Finding #16: counting `chars()` wrapped CJK at twice the intended
    /// column count.
    #[test]
    fn char_wrap_measures_wide_characters_in_columns() {
        // Six columns of CJK = three characters, not six.
        assert_eq!(char_wrap("日本語", 6), vec!["日本語"]);
        assert_eq!(char_wrap("日本語", 4), vec!["日本", "語"]);
    }

    #[test]
    fn char_wrap_terminates_when_a_char_is_wider_than_the_budget() {
        // Budget 1 with 2-column characters: each still gets its own line.
        assert_eq!(char_wrap("日本", 1), vec!["日", "本"]);
    }

    // ── word_wrap ───────────────────────────────────────────────────────────

    #[test]
    fn preserves_double_space_between_words() {
        assert_eq!(word_wrap("foo  bar", 20), vec!["foo  bar"]);
    }

    #[test]
    fn preserves_multiple_spaces_between_words() {
        assert_eq!(word_wrap("a  b   c", 20), vec!["a  b   c"]);
    }

    #[test]
    fn drops_double_space_at_wrap_boundary() {
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
        assert_eq!(word_wrap("foo  bar\nbaz  qux", 20), vec!["foo  bar", "baz  qux"]);
    }

    #[test]
    fn blank_line_preserved() {
        assert_eq!(word_wrap("foo\n\nbar", 20), vec!["foo", "", "bar"]);
    }

    #[test]
    fn words_fill_exact_width_per_line() {
        assert_eq!(word_wrap("ab cd ef", 5), vec!["ab cd", "ef"]);
    }

    #[test]
    fn three_words_wrap_at_boundary() {
        assert_eq!(word_wrap("one two three", 7), vec!["one two", "three"]);
    }

    #[test]
    fn single_word_never_wraps() {
        assert_eq!(word_wrap("hello", 10), vec!["hello"]);
    }

    /// Finding #16: a CJK title wrapped at half the intended column count and
    /// overflowed the pane.
    #[test]
    fn word_wrap_measures_wide_characters_in_columns() {
        // Each character is 2 columns, so 10 columns holds five of them.
        let text = "日本語日本語日本";
        let lines = word_wrap(text, 10);
        assert_eq!(lines, vec!["日本語日本", "語日本"]);
        for line in &lines {
            assert!(display_width(line) <= 10, "{line:?} overflows");
        }
    }

    #[test]
    fn word_wrap_hard_breaks_an_overlong_cjk_word_by_columns() {
        let lines = word_wrap("日本語", 4);
        assert_eq!(lines, vec!["日本", "語"]);
    }

    // ── wrap_rows ───────────────────────────────────────────────────────────

    #[test]
    fn wrap_rows_keeps_short_text_whole() {
        assert_eq!(wrap_rows("abc", 10), vec!["abc"]);
        assert_eq!(wrap_rows("", 10), vec![""]);
        assert_eq!(wrap_rows("abc", 0), vec!["abc"]);
    }

    #[test]
    fn wrap_rows_breaks_on_spaces_dropping_them() {
        assert_eq!(wrap_rows("alpha beta gamma delta", 11), vec!["alpha beta", "gamma delta"]);
    }

    #[test]
    fn wrap_rows_hard_breaks_a_word_with_no_space() {
        assert_eq!(wrap_rows("abcdefghijklmnop", 5), vec!["abcde", "fghij", "klmno", "p"]);
    }

    #[test]
    fn wrap_rows_counts_wide_characters_as_two_columns() {
        assert_eq!(wrap_rows("→→→→→ x", 5), vec!["→→→→→", "x"]);
        assert_eq!(wrap_rows("日本語です", 4), vec!["日本", "語で", "す"]);
    }

    #[test]
    fn wrap_rows_makes_progress_for_an_over_wide_character() {
        assert_eq!(wrap_rows("日本", 1), vec!["日", "本"]);
    }

    #[test]
    fn wrap_rows_rows_never_exceed_the_budget() {
        for text in ["alpha beta gamma", "日本語のタイトルです", "🚀🚀🚀 🚀", "a  b   c"] {
            for width in 2..=20usize {
                for row in wrap_rows(text, width) {
                    // A single character wider than the whole budget is
                    // emitted on its own row rather than dropped — that is
                    // the one allowed overflow, and it is one character long.
                    let single_char_overflow = row.chars().count() == 1;
                    assert!(
                        display_width(&row) <= width || single_char_overflow,
                        "{text:?} at {width}: {row:?} is {} columns",
                        display_width(&row)
                    );
                }
            }
        }
    }

    // ── split_at_width ──────────────────────────────────────────────────────

    #[test]
    fn split_at_width_prefers_the_last_space_and_keeps_it() {
        assert_eq!(split_at_width("foo bar baz", 8), ("foo bar ", "baz"));
    }

    #[test]
    fn split_at_width_hard_breaks_when_there_is_no_space() {
        assert_eq!(split_at_width("abcdefgh", 3), ("abc", "defgh"));
    }

    #[test]
    fn split_at_width_measures_columns_for_wide_text() {
        // Four columns of CJK = two characters.
        assert_eq!(split_at_width("日本語", 4), ("日本", "語"));
    }

    #[test]
    fn split_at_width_makes_progress_for_an_over_wide_character() {
        // A 2-column character with a 1-column budget still advances.
        assert_eq!(split_at_width("日本", 1), ("日", "本"));
        assert_eq!(split_at_width("abc", 0), ("a", "bc"));
    }

    #[test]
    fn split_at_width_empty_input() {
        assert_eq!(split_at_width("", 5), ("", ""));
    }

    // ── hint_height ─────────────────────────────────────────────────────────

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
        assert_eq!(hint_height(&"a".repeat(80), 80), 1);
    }

    #[test]
    fn hint_height_two_rows_when_text_spans_two_widths() {
        assert_eq!(hint_height(&"a".repeat(100), 50), 2);
    }

    #[test]
    fn hint_height_is_capped_at_three() {
        assert_eq!(hint_height(&"a".repeat(400), 50), 3);
    }

    /// Finding #16: `chars().count()` under-counted wide text, so the hint bar
    /// was given fewer rows than the wrapped text needed.
    #[test]
    fn hint_height_counts_wide_characters_as_two_columns() {
        // 40 CJK characters = 80 columns = two rows at width 50.
        let text = "日".repeat(40);
        assert_eq!(hint_height(&text, 50), 2);
        // Character counting would have said one row.
        assert_eq!(text.chars().count(), 40);
    }

    // ── find_matches / split_matches (finding #17) ──────────────────────────

    fn q(s: &str) -> Vec<char> {
        s.to_lowercase().chars().collect()
    }

    fn matched_text(text: &str, query: &str) -> Vec<String> {
        split_matches(text, &q(query))
            .into_iter()
            .filter(|(_, is_match)| *is_match)
            .map(|(seg, _)| seg)
            .collect()
    }

    #[test]
    fn ascii_matches_are_found_case_insensitively() {
        assert_eq!(matched_text("Hello World", "world"), vec!["World"]);
        assert_eq!(matched_text("aaa", "a"), vec!["a", "a", "a"]);
        assert_eq!(matched_text("abc", "zzz"), Vec::<String>::new());
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        assert_eq!(find_matches("abc", &[]), Vec::new());
        assert_eq!(split_matches("abc", &[]), vec![("abc".to_string(), false)]);
    }

    /// The regression: `'İ'` lowercases to two chars, so offsets taken from the
    /// lowercased string highlighted the wrong region of the original.
    #[test]
    fn a_dotless_capital_i_does_not_shift_the_match() {
        assert_eq!(matched_text("İstanbul", "i"), vec!["İ"]);
        assert_eq!(matched_text("İstanbul", "İ"), vec!["İ"]);
        // A match that starts *after* the two-char fold of `İ`: the old code
        // sliced the original at the lowercased offset and reported `tanb`.
        assert_eq!(matched_text("İstanbul", "stan"), vec!["stan"]);
    }

    #[test]
    fn accented_text_matches_without_drift() {
        assert_eq!(matched_text("Café", "café"), vec!["Café"]);
        assert_eq!(matched_text("STRASSE", "strasse"), vec!["STRASSE"]);
    }

    #[test]
    fn the_segments_always_reconstruct_the_original_text() {
        for (text, query) in [
            ("İstanbul", "i"),
            ("İstanbul", "İ"),
            ("hello world", "o"),
            ("no match here", "zzz"),
            ("", "a"),
            ("日本語テキスト", "語"),
        ] {
            let joined: String = split_matches(text, &q(query))
                .into_iter()
                .map(|(seg, _)| seg)
                .collect();
            assert_eq!(joined, text, "query {query:?}");
        }
    }

    #[test]
    fn a_match_never_splits_a_character() {
        for (text, query) in [("İstanbul", "i"), ("İstanbul", "İ"), ("日本語", "本")] {
            for (seg, is_match) in split_matches(text, &q(query)) {
                if is_match {
                    // The matched segment must be made of whole original
                    // characters: re-lowercasing it must contain the query.
                    assert!(
                        seg.to_lowercase().contains(&query.to_lowercase()),
                        "{seg:?} does not contain {query:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn wide_text_matches_by_character_not_column() {
        assert_eq!(matched_text("日本語", "本"), vec!["本"]);
        assert_eq!(matched_text("日本語", "日本"), vec!["日本"]);
    }
}
