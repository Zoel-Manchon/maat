//! Text as a vector of lines.
//!
//! Serious editors use a *rope* (a tree structure) so that editing the middle
//! of a 100 MB file doesn't copy memory around. A `Vec<String>` is the right
//! call at this scope: simple, obvious, and plenty up to tens of thousands of
//! lines. Swapping it later stays an internal detail, because the rest of the
//! editor only ever talks to this API.

/// Public indices are always in **characters**, never bytes: in UTF-8 an `é`
/// takes two bytes, and slicing at the wrong one yields corrupt text (or a
/// panic). The conversion to byte offsets is contained in here.
#[derive(Debug, Clone, PartialEq)]
pub struct Buffer {
    lines: Vec<String>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self { lines: vec![String::new()] }
    }
}

impl Buffer {
    /// A buffer always holds at least one line (possibly empty): an invariant
    /// that simplifies every bit of cursor and rendering code.
    pub fn from_text(text: &str) -> Self {
        let lines: Vec<String> = text.split('\n').map(String::from).collect();
        if lines.is_empty() {
            Self::default()
        } else {
            Self { lines }
        }
    }

    pub fn to_text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, row: usize) -> Option<&str> {
        self.lines.get(row).map(String::as_str)
    }

    /// Length in characters, not bytes.
    pub fn line_len(&self, row: usize) -> usize {
        self.lines.get(row).map_or(0, |line| line.chars().count())
    }

    pub fn insert_char(&mut self, row: usize, col: usize, ch: char) {
        if let Some(line) = self.lines.get_mut(row) {
            let byte = char_to_byte(line, col);
            line.insert(byte, ch);
        }
    }

    /// Deletes the character at `col`. Returns `true` if anything was removed.
    pub fn delete_char(&mut self, row: usize, col: usize) -> bool {
        let Some(line) = self.lines.get_mut(row) else { return false };
        if col >= line.chars().count() {
            return false;
        }
        let byte = char_to_byte(line, col);
        line.remove(byte);
        true
    }

    /// Splits the line in two at `col` (what Enter does in insert mode).
    pub fn insert_newline(&mut self, row: usize, col: usize) {
        if row >= self.lines.len() {
            return;
        }
        let byte = char_to_byte(&self.lines[row], col);
        let tail = self.lines[row].split_off(byte);
        self.lines.insert(row + 1, tail);
    }

    /// Appends line `row + 1` to the end of `row` (backspace at column zero).
    /// Returns `true` if a join happened.
    pub fn join_line(&mut self, row: usize) -> bool {
        if row + 1 >= self.lines.len() {
            return false;
        }
        let next = self.lines.remove(row + 1);
        self.lines[row].push_str(&next);
        true
    }

    /// Removes a whole line (`dd`). Never leaves the buffer with zero lines.
    pub fn delete_line(&mut self, row: usize) -> bool {
        if row >= self.lines.len() {
            return false;
        }
        self.lines.remove(row);
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        true
    }

    pub fn insert_line(&mut self, row: usize, text: String) {
        let at = row.min(self.lines.len());
        self.lines.insert(at, text);
    }

    pub fn iter_lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }
}

/// Translates a character index into a byte offset within the line.
/// If `col` lands past the end, it returns the end: appending is a valid
/// operation, not an error.
fn char_to_byte(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map_or(line.len(), |(byte, _)| byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_has_one_line() {
        let buffer = Buffer::default();
        assert_eq!(buffer.line_count(), 1);
        assert_eq!(buffer.line(0), Some(""));
    }

    #[test]
    fn roundtrips_text() {
        let buffer = Buffer::from_text("one\ntwo\nthree");
        assert_eq!(buffer.line_count(), 3);
        assert_eq!(buffer.line(1), Some("two"));
        assert_eq!(buffer.to_text(), "one\ntwo\nthree");
    }

    #[test]
    fn inserts_and_deletes_chars() {
        let mut buffer = Buffer::from_text("ab");
        buffer.insert_char(0, 1, 'X');
        assert_eq!(buffer.line(0), Some("aXb"));

        assert!(buffer.delete_char(0, 1));
        assert_eq!(buffer.line(0), Some("ab"));
        assert!(!buffer.delete_char(0, 9));
    }

    #[test]
    fn handles_multibyte_chars() {
        // "café" is 4 characters but 5 bytes: slicing by byte would break the é.
        let mut buffer = Buffer::from_text("café");
        assert_eq!(buffer.line_len(0), 4);

        buffer.insert_char(0, 4, 's');
        assert_eq!(buffer.line(0), Some("cafés"));

        assert!(buffer.delete_char(0, 3));
        assert_eq!(buffer.line(0), Some("cafs"));
    }

    #[test]
    fn splits_and_joins_lines() {
        let mut buffer = Buffer::from_text("hello world");
        buffer.insert_newline(0, 5);
        assert_eq!(buffer.line(0), Some("hello"));
        assert_eq!(buffer.line(1), Some(" world"));

        assert!(buffer.join_line(0));
        assert_eq!(buffer.line(0), Some("hello world"));
        assert_eq!(buffer.line_count(), 1);
        assert!(!buffer.join_line(0));
    }

    #[test]
    fn deleting_last_line_keeps_buffer_usable() {
        let mut buffer = Buffer::from_text("only");
        assert!(buffer.delete_line(0));
        assert_eq!(buffer.line_count(), 1);
        assert_eq!(buffer.line(0), Some(""));
    }
}
