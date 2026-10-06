use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Cursor-aware single-line input state.
///
/// Tracks both the string value and the cursor position as a character index
/// (not a byte offset).  All mutation methods keep `cursor` within bounds.
#[derive(Clone, Debug, Default)]
pub struct InputState {
    /// The full string being edited.
    pub value: String,
    /// Cursor position as a character index (0 = before the first char).
    cursor: usize,
}

impl InputState {
    /// Create an empty input with the cursor at position 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an input pre-populated with `s`, cursor placed at the end.
    pub fn from_str(s: &str) -> Self {
        Self { value: s.to_owned(), cursor: s.chars().count() }
    }

    /// Return the value trimmed of leading/trailing whitespace (for saving).
    pub fn trimmed(&self) -> &str {
        self.value.trim()
    }

    // ─── Mutation ─────────────────────────────────────────────────────────

    /// Insert `c` at the cursor and advance the cursor by one.
    pub fn insert(&mut self, c: char) {
        let byte_pos = self.char_to_byte(self.cursor);
        self.value.insert(byte_pos, c);
        self.cursor += 1;
    }

    /// Delete the character immediately before the cursor (Backspace).
    pub fn delete_back(&mut self) {
        if self.cursor == 0 { return; }
        let end_byte   = self.char_to_byte(self.cursor);
        let start_byte = self.char_to_byte(self.cursor - 1);
        self.value.drain(start_byte..end_byte);
        self.cursor -= 1;
    }

    /// Delete the character at the cursor position (Delete / Forward-delete).
    pub fn delete_forward(&mut self) {
        let len = self.char_len();
        if self.cursor >= len { return; }
        let start_byte = self.char_to_byte(self.cursor);
        let end_byte   = self.char_to_byte(self.cursor + 1);
        self.value.drain(start_byte..end_byte);
        // cursor stays at the same index
    }

    // ─── Cursor movement ──────────────────────────────────────────────────

    pub fn move_left(&mut self) {
        if self.cursor > 0 { self.cursor -= 1; }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.char_len() { self.cursor += 1; }
    }

    pub fn home(&mut self) { self.cursor = 0; }
    pub fn end(&mut self)  { self.cursor = self.char_len(); }

    // ─── Rendering ────────────────────────────────────────────────────────

    /// Decompose the visible window into `(before, cursor_char, after)` spans.
    ///
    /// `width` is the number of character columns available for display.
    /// The window slides automatically to keep the cursor visible at all times.
    /// When the cursor is past the end of the text, `cursor_char` is `' '`
    /// (a placeholder for a block cursor).
    pub fn display(&self, width: usize) -> (String, char, String) {
        if width == 0 {
            return (String::new(), ' ', String::new());
        }

        let chars: Vec<char> = self.value.chars().collect();
        let len = chars.len();

        // Slide the window so the cursor stays within [scroll, scroll + width).
        let scroll = (self.cursor + 1).saturating_sub(width);

        let before_count = self.cursor - scroll;          // always in 0..=width-1
        let after_cap    = width.saturating_sub(before_count + 1);

        let before: String = chars[scroll..scroll + before_count].iter().collect();

        let (cursor_char, after_start) = if self.cursor < len {
            (chars[self.cursor], self.cursor + 1)
        } else {
            (' ', len)
        };

        let after_end = (after_start + after_cap).min(len);
        let after: String = if after_start < after_end {
            chars[after_start..after_end].iter().collect()
        } else {
            String::new()
        };

        (before, cursor_char, after)
    }

    // ─── Private helpers ──────────────────────────────────────────────────

    fn char_len(&self) -> usize {
        self.value.chars().count()
    }

    /// Convert a character index into a byte offset within `self.value`.
    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.value.len())
    }
}

// ─── Key dispatch ─────────────────────────────────────────────────────────────

/// The outcome of [`handle_input_key`].
#[derive(Debug, PartialEq, Eq)]
pub enum InputKeyResult {
    /// The user pressed Enter; the caller should save the value.
    Enter,
    /// The user pressed Escape; the caller should discard the edit.
    Escape,
    /// The edit is still in progress; the caller should keep the state alive.
    Continue,
}

/// Route a key event to the appropriate [`InputState`] mutation.
///
/// Recognised keys:
/// - `Char(c)` — insert (non-control characters only)
/// - `Backspace` — delete before cursor
/// - `Delete` — delete at cursor
/// - `Left` / `Right` — move cursor
/// - `Home` / `End` — jump to start / end
/// - `Ctrl+A` / `Ctrl+E` — readline-style home / end
/// - `Enter` — signal save
/// - `Esc` — signal discard
pub fn handle_input_key(state: &mut InputState, key: KeyEvent) -> InputKeyResult {
    match key.code {
        KeyCode::Enter => return InputKeyResult::Enter,
        KeyCode::Esc   => return InputKeyResult::Escape,

        KeyCode::Left  => state.move_left(),
        KeyCode::Right => state.move_right(),
        KeyCode::Home  => state.home(),
        KeyCode::End   => state.end(),

        // Readline-style home / end.
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => state.home(),
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => state.end(),

        // Regular character insertion — ignore control / alt combos.
        KeyCode::Char(c)
            if !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            state.insert(c);
        }

        KeyCode::Backspace => state.delete_back(),
        KeyCode::Delete    => state.delete_forward(),

        _ => {}
    }
    InputKeyResult::Continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    // ── Construction ───────────────────────────────────────────────────────────────

    #[test]
    fn new_is_empty() {
        let s = InputState::new();
        assert!(s.value.is_empty());
        assert_eq!(s.trimmed(), "");
    }

    #[test]
    fn from_str_populates_value_and_places_cursor_at_end() {
        // Cursor at end means delete_back removes the last char.
        let mut s = InputState::from_str("hello");
        s.delete_back();
        assert_eq!(s.value, "hell");
    }

    #[test]
    fn trimmed_strips_surrounding_whitespace() {
        let s = InputState::from_str("  hello  ");
        assert_eq!(s.trimmed(), "hello");
    }

    // ── Insertion ───────────────────────────────────────────────────────────────────

    #[test]
    fn insert_appends_when_cursor_at_end() {
        let mut s = InputState::new();
        s.insert('a');
        s.insert('b');
        s.insert('c');
        assert_eq!(s.value, "abc");
    }

    #[test]
    fn insert_in_middle_shifts_trailing_chars() {
        let mut s = InputState::from_str("ac");
        s.move_left(); // cursor moves before 'c'
        s.insert('b');
        assert_eq!(s.value, "abc");
    }

    #[test]
    fn insert_handles_multibyte_unicode() {
        let mut s = InputState::new();
        s.insert('\u{00e9}'); // é (2 bytes)
        s.insert('\u{00e0}'); // à (2 bytes)
        s.insert('\u{00fc}'); // ü (2 bytes)
        assert_eq!(s.value, "\u{00e9}\u{00e0}\u{00fc}");
    }

    // ── Deletion ────────────────────────────────────────────────────────────────────

    #[test]
    fn delete_back_removes_char_before_cursor() {
        let mut s = InputState::from_str("hello");
        s.delete_back();
        assert_eq!(s.value, "hell");
    }

    #[test]
    fn delete_back_at_start_is_noop() {
        let mut s = InputState::from_str("hi");
        s.home();
        s.delete_back();
        assert_eq!(s.value, "hi");
    }

    #[test]
    fn delete_back_removes_multibyte_char_as_single_unit() {
        let mut s = InputState::from_str("h\u{00e9}"); // hé
        s.delete_back(); // removes 'é' (2 UTF-8 bytes, 1 char)
        assert_eq!(s.value, "h");
    }

    #[test]
    fn delete_forward_removes_char_at_cursor() {
        let mut s = InputState::from_str("hello");
        s.home();
        s.delete_forward();
        assert_eq!(s.value, "ello");
    }

    #[test]
    fn delete_forward_at_end_is_noop() {
        let mut s = InputState::from_str("hi");
        // from_str leaves cursor at end
        s.delete_forward();
        assert_eq!(s.value, "hi");
    }

    #[test]
    fn delete_forward_removes_multibyte_char_as_single_unit() {
        let mut s = InputState::from_str("\u{00e9}b"); // éb
        s.home();
        s.delete_forward(); // removes 'é'
        assert_eq!(s.value, "b");
    }

    // ── Cursor movement ───────────────────────────────────────────────────────────

    #[test]
    fn move_left_at_start_does_not_underflow() {
        let mut s = InputState::new();
        s.move_left();
        s.move_left();
        // Still at position 0: insert goes to the front.
        s.insert('x');
        assert_eq!(s.value, "x");
    }

    #[test]
    fn move_right_at_end_stays_at_end() {
        let mut s = InputState::from_str("ab");
        s.move_right();
        s.move_right(); // already at end; no change
        // Still at end: delete_back removes 'b'.
        s.delete_back();
        assert_eq!(s.value, "a");
    }

    #[test]
    fn home_positions_cursor_at_start() {
        let mut s = InputState::from_str("hello");
        s.home();
        s.delete_back(); // noop at start
        assert_eq!(s.value, "hello");
        s.insert('X');
        assert_eq!(s.value, "Xhello");
    }

    #[test]
    fn end_positions_cursor_at_end() {
        let mut s = InputState::from_str("hello");
        s.home();
        s.end();
        s.delete_forward(); // noop at end
        assert_eq!(s.value, "hello");
        s.delete_back(); // removes 'o'
        assert_eq!(s.value, "hell");
    }

    // ── display() ────────────────────────────────────────────────────────────────────

    #[test]
    fn display_empty_input_gives_placeholder_cursor() {
        let s = InputState::new();
        let (before, cur, after) = s.display(10);
        assert_eq!(before, "");
        assert_eq!(cur, ' ');
        assert_eq!(after, "");
    }

    #[test]
    fn display_zero_width_returns_empty_placeholder() {
        let s = InputState::from_str("hello");
        let (before, cur, after) = s.display(0);
        assert_eq!((before.as_str(), cur, after.as_str()), ("", ' ', ""));
    }

    #[test]
    fn display_cursor_at_start() {
        let mut s = InputState::from_str("hello");
        s.home();
        let (before, cur, after) = s.display(10);
        assert_eq!(before, "");
        assert_eq!(cur, 'h');
        assert_eq!(after, "ello");
    }

    #[test]
    fn display_cursor_at_end_shows_placeholder() {
        // from_str places cursor past the last char.
        let s = InputState::from_str("hi");
        let (before, cur, after) = s.display(10);
        assert_eq!(before, "hi");
        assert_eq!(cur, ' ');
        assert_eq!(after, "");
    }

    #[test]
    fn display_cursor_in_middle_splits_correctly() {
        let mut s = InputState::from_str("hello");
        s.home();
        s.move_right(); // pos 1
        s.move_right(); // pos 2
        let (before, cur, after) = s.display(20);
        assert_eq!(before, "he");
        assert_eq!(cur, 'l');
        assert_eq!(after, "lo");
    }

    #[test]
    fn display_scrolls_when_cursor_beyond_width() {
        // "abcde", cursor at end (5), width = 3.
        // scroll = (5+1).saturating_sub(3) = 3
        // before = chars[3..5] = "de", cursor_char = ' '
        let s = InputState::from_str("abcde");
        let (before, cur, after) = s.display(3);
        assert_eq!(before, "de");
        assert_eq!(cur, ' ');
        assert_eq!(after, "");
    }

    #[test]
    fn display_scrolls_with_cursor_mid_string() {
        // "abcde", cursor at 3 ('d'), width = 3.
        // scroll = (3+1).saturating_sub(3) = 1
        // before = chars[1..3] = "bc", cursor_char = 'd', after_cap = 0
        let mut s = InputState::from_str("abcde");
        s.home();
        s.move_right(); // 1
        s.move_right(); // 2
        s.move_right(); // 3
        let (before, cur, after) = s.display(3);
        assert_eq!(before, "bc");
        assert_eq!(cur, 'd');
        assert_eq!(after, "");
    }

    // ── handle_input_key ───────────────────────────────────────────────────────────

    #[test]
    fn enter_key_returns_enter_signal() {
        let mut s = InputState::new();
        assert_eq!(handle_input_key(&mut s, key(KeyCode::Enter)), InputKeyResult::Enter);
    }

    #[test]
    fn escape_key_returns_escape_signal() {
        let mut s = InputState::new();
        assert_eq!(handle_input_key(&mut s, key(KeyCode::Esc)), InputKeyResult::Escape);
    }

    #[test]
    fn char_key_inserts_and_returns_continue() {
        let mut s = InputState::new();
        let result = handle_input_key(&mut s, key(KeyCode::Char('z')));
        assert_eq!(result, InputKeyResult::Continue);
        assert_eq!(s.value, "z");
    }

    #[test]
    fn backspace_key_deletes_before_cursor() {
        let mut s = InputState::from_str("hello");
        handle_input_key(&mut s, key(KeyCode::Backspace));
        assert_eq!(s.value, "hell");
    }

    #[test]
    fn delete_key_deletes_at_cursor() {
        let mut s = InputState::from_str("hello");
        handle_input_key(&mut s, key(KeyCode::Home));
        handle_input_key(&mut s, key(KeyCode::Delete));
        assert_eq!(s.value, "ello");
    }

    #[test]
    fn left_right_arrow_keys_move_cursor() {
        let mut s = InputState::from_str("ab");
        // Move to start.
        handle_input_key(&mut s, key(KeyCode::Left));
        handle_input_key(&mut s, key(KeyCode::Left));
        handle_input_key(&mut s, key(KeyCode::Char('X')));
        assert_eq!(s.value, "Xab");
        // Move to end.
        handle_input_key(&mut s, key(KeyCode::Right));
        handle_input_key(&mut s, key(KeyCode::Right));
        handle_input_key(&mut s, key(KeyCode::Right)); // already at end
        handle_input_key(&mut s, key(KeyCode::Char('Y')));
        assert_eq!(s.value, "XabY");
    }

    #[test]
    fn home_end_keys_jump_to_bounds() {
        let mut s = InputState::from_str("hello");
        handle_input_key(&mut s, key(KeyCode::Home));
        handle_input_key(&mut s, key(KeyCode::Char('X')));
        assert_eq!(s.value, "Xhello");
        handle_input_key(&mut s, key(KeyCode::End));
        handle_input_key(&mut s, key(KeyCode::Char('Y')));
        assert_eq!(s.value, "XhelloY");
    }

    #[test]
    fn ctrl_a_jumps_to_home() {
        let mut s = InputState::from_str("hello");
        handle_input_key(&mut s, ctrl_key('a'));
        handle_input_key(&mut s, key(KeyCode::Char('X')));
        assert_eq!(s.value, "Xhello");
    }

    #[test]
    fn ctrl_e_jumps_to_end() {
        let mut s = InputState::from_str("hello");
        handle_input_key(&mut s, key(KeyCode::Home));
        handle_input_key(&mut s, ctrl_key('e'));
        handle_input_key(&mut s, key(KeyCode::Backspace));
        assert_eq!(s.value, "hell");
    }

    #[test]
    fn ctrl_modifier_suppresses_character_insertion() {
        let mut s = InputState::new();
        // Ctrl+B is unhandled; it must not insert 'b'.
        handle_input_key(&mut s, ctrl_key('b'));
        assert_eq!(s.value, "");
    }

    #[test]
    fn alt_modifier_suppresses_character_insertion() {
        let mut s = InputState::new();
        let alt_char = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
        handle_input_key(&mut s, alt_char);
        assert_eq!(s.value, "");
    }
}
