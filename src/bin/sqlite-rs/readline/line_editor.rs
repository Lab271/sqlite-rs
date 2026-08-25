//! In-memory line buffer with cursor-relative editing operations (#558).
//! Operates on `char`s (not bytes), so multi-byte UTF-8 input is edited
//! and measured correctly.

/// A single line under edit: the text plus the cursor's position as a
/// char index (0..=chars.len()).
#[derive(Default)]
pub struct LineEditor {
    chars: Vec<char>,
    cursor: usize,
}

impl LineEditor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_str(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn len(&self) -> usize {
        self.chars.len()
    }

    /// Replaces the whole buffer (used when recalling history) and
    /// moves the cursor to the end.
    pub fn set(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn insert(&mut self, c: char) {
        self.chars.insert(self.cursor, c);
        self.cursor = self.cursor.saturating_add(1);
    }

    /// Backspace: deletes the char before the cursor.
    pub fn delete_before(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.cursor.saturating_sub(1);
            self.chars.remove(self.cursor);
        }
    }

    /// Delete key: deletes the char under the cursor.
    pub fn delete_at(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor = self.cursor.saturating_add(1);
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Deletes from the start of the line up to (not including) the
    /// cursor — Ctrl-U.
    pub fn delete_to_home(&mut self) {
        self.chars.drain(0..self.cursor);
        self.cursor = 0;
    }

    /// Deletes from the cursor to the end of the line — Ctrl-K.
    pub fn delete_to_end(&mut self) {
        self.chars.truncate(self.cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_moves_cursor_forward() {
        let mut e = LineEditor::new();
        e.insert('a');
        e.insert('b');
        assert_eq!(e.as_str(), "ab");
        assert_eq!(e.cursor(), 2);
    }

    #[test]
    fn delete_before_at_start_is_noop() {
        let mut e = LineEditor::new();
        e.delete_before();
        assert_eq!(e.as_str(), "");
    }

    #[test]
    fn insert_in_middle() {
        let mut e = LineEditor::new();
        e.set("ac");
        e.move_left();
        e.insert('b');
        assert_eq!(e.as_str(), "abc");
    }

    #[test]
    fn home_and_end() {
        let mut e = LineEditor::new();
        e.set("hello");
        e.move_home();
        assert_eq!(e.cursor(), 0);
        e.move_end();
        assert_eq!(e.cursor(), 5);
    }

    #[test]
    fn delete_to_home_and_end() {
        let mut e = LineEditor::new();
        e.set("hello world");
        e.cursor = 6;
        e.delete_to_home();
        assert_eq!(e.as_str(), "world");
        e.set("hello world");
        e.cursor = 5;
        e.delete_to_end();
        assert_eq!(e.as_str(), "hello");
    }

    #[test]
    fn multi_byte_chars_count_as_one() {
        let mut e = LineEditor::new();
        e.set("héllo");
        assert_eq!(e.len(), 5);
        e.move_home();
        e.delete_at();
        assert_eq!(e.as_str(), "éllo");
    }
}
