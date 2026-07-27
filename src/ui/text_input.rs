//! A single-line text field with an insertion cursor, used by every dialog so arrow keys,
//! Home/End and Delete all work. Pure logic (no ratatui), so the cursor math is unit-tested.

/// A line of editable text with a cursor (a char index in `0..=char_len`).
#[derive(Clone)]
pub struct TextInput {
    text: String,
    cursor: usize,
    /// When set, the next inserted character first clears the field. Lets a prefilled
    /// dialog (a default value or an existing name) be replaced by just typing — but moving
    /// the cursor first (arrow/Home/End) commits to editing instead of replacing.
    fresh: bool,
}

impl TextInput {
    /// A field prefilled with `initial`, cursor at the end, editing mode (no auto-erase).
    pub fn new(initial: impl Into<String>) -> Self {
        let text = initial.into();
        let cursor = text.chars().count();
        Self { text, cursor, fresh: false }
    }

    /// Like `new`, but the next typed character replaces the prefilled value (unless the
    /// cursor is moved first).
    pub fn fresh(initial: impl Into<String>) -> Self {
        let mut s = Self::new(initial);
        s.fresh = true;
        s
    }

    pub fn value(&self) -> &str {
        &self.text
    }

    fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    fn byte_at(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    pub fn insert(&mut self, c: char) {
        if self.fresh {
            self.text.clear();
            self.cursor = 0;
            self.fresh = false;
        }
        let b = self.byte_at(self.cursor);
        self.text.insert(b, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        self.fresh = false;
        if self.cursor > 0 {
            let b = self.byte_at(self.cursor - 1);
            self.text.remove(b);
            self.cursor -= 1;
        }
    }

    pub fn delete(&mut self) {
        self.fresh = false;
        if self.cursor < self.char_len() {
            let b = self.byte_at(self.cursor);
            self.text.remove(b);
        }
    }

    pub fn left(&mut self) {
        self.fresh = false;
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.fresh = false;
        self.cursor = (self.cursor + 1).min(self.char_len());
    }

    pub fn home(&mut self) {
        self.fresh = false;
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.fresh = false;
        self.cursor = self.char_len();
    }

    /// Places the cursor at character offset `column` — how a mouse click inside the field
    /// lands the caret where it was clicked rather than merely focusing the field. `column` is
    /// the click's column *within the text itself*, so the caller subtracts whatever label or
    /// padding precedes it.
    ///
    /// Clamped to the end of the text: clicking in the empty space past the last character is
    /// a click at the end, which is what every text field everywhere does. Clears `fresh` for
    /// the same reason the arrow keys do — deliberately positioning the caret is a commitment
    /// to editing the existing value, not to replacing it wholesale.
    pub fn set_cursor_from_column(&mut self, column: usize) {
        self.fresh = false;
        self.cursor = column.min(self.char_len());
    }

    /// Splits the text for rendering a cursor: `(before, under_cursor, after)`. The middle
    /// is the single character the cursor sits on, or a space when the cursor is at the end.
    pub fn split_at_cursor(&self) -> (String, String, String) {
        let chars: Vec<char> = self.text.chars().collect();
        let before: String = chars[..self.cursor].iter().collect();
        let under: String = chars.get(self.cursor).map(|c| c.to_string()).unwrap_or_else(|| " ".to_string());
        // `get(cursor+1..)` already yields None past the end (→ empty), so no length guard
        // is needed: it correctly returns "" when the cursor is at or on the last char.
        let after: String = chars.get(self.cursor + 1..).map(|s| s.iter().collect()).unwrap_or_default();
        (before, under, after)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_appends_at_cursor() {
        let mut t = TextInput::new("");
        for c in "abc".chars() {
            t.insert(c);
        }
        assert_eq!(t.value(), "abc");
        // Cursor sits at the end → nothing "under" it.
        assert_eq!(t.split_at_cursor().1, " ");
    }

    #[test]
    fn fresh_field_is_cleared_by_first_type_but_not_by_arrows() {
        let mut t = TextInput::fresh("default.wav");
        t.insert('x');
        assert_eq!(t.value(), "x");

        // Moving the cursor first cancels the auto-erase.
        let mut t2 = TextInput::fresh("default.wav");
        t2.left();
        t2.insert('x');
        assert_eq!(t2.value(), "default.waxv");
    }

    #[test]
    fn arrows_move_and_edit_mid_string() {
        let mut t = TextInput::new("abc");
        t.left(); // between b and c
        t.insert('X');
        assert_eq!(t.value(), "abXc");
        t.home();
        t.insert('Y');
        assert_eq!(t.value(), "YabXc");
        t.end();
        t.backspace();
        assert_eq!(t.value(), "YabX");
        t.home();
        t.delete();
        assert_eq!(t.value(), "abX");
    }

    /// Clicking inside a field puts the caret on the clicked character, and clicking past the
    /// end lands at the end rather than out of bounds.
    #[test]
    fn clicking_positions_the_cursor_and_clamps_past_the_end() {
        let mut t = TextInput::new("abcdef");
        t.set_cursor_from_column(2);
        t.insert('X');
        assert_eq!(t.value(), "abXcdef");

        t.set_cursor_from_column(999);
        t.insert('Z');
        assert_eq!(t.value(), "abXcdefZ", "a click past the text ends up at the end");
    }

    /// Deliberately positioning the caret is a commitment to editing the prefilled value, not
    /// replacing it — the same rule the arrow keys already follow.
    #[test]
    fn clicking_a_fresh_field_cancels_its_auto_erase() {
        let mut t = TextInput::fresh("default.wav");
        t.set_cursor_from_column(0);
        t.insert('x');
        assert_eq!(t.value(), "xdefault.wav");
    }

    /// Char offsets, not byte offsets: clicking column 2 of a multi-byte string must land
    /// between the second and third *characters*, not split one.
    #[test]
    fn click_positioning_counts_characters_not_bytes() {
        let mut t = TextInput::new("äöü.wav");
        t.set_cursor_from_column(2);
        t.insert('X');
        assert_eq!(t.value(), "äöXü.wav");
    }

    #[test]
    fn split_at_cursor_marks_position() {
        let mut t = TextInput::new("abc");
        t.left();
        let (before, under, after) = t.split_at_cursor();
        assert_eq!((before.as_str(), under.as_str(), after.as_str()), ("ab", "c", ""));
        t.end();
        let (before, under, after) = t.split_at_cursor();
        assert_eq!((before.as_str(), under.as_str(), after.as_str()), ("abc", " ", ""));
    }
}
