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

    /// Swaps a whole line for new contents. What `:s` writes back after it has
    /// rewritten a line; out-of-range rows are ignored rather than panicking,
    /// like every other mutator here.
    pub fn replace_line(&mut self, row: usize, text: String) -> bool {
        let Some(line) = self.lines.get_mut(row) else { return false };
        *line = text;
        true
    }

    pub fn iter_lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(String::as_str)
    }

    /// The text of an **inclusive** character range, `(row, col)` to
    /// `(row, col)`.
    ///
    /// Inclusive because that is what a visual selection means: the character
    /// under the cursor is part of it, which is the difference between `d`
    /// deleting what you highlighted and deleting one character less.
    pub fn range_text(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let (start, end) = order(start, end);
        if start.0 >= self.lines.len() {
            return String::new();
        }
        let last_row = end.0.min(self.lines.len().saturating_sub(1));

        if start.0 == last_row {
            let line = &self.lines[start.0];
            return slice_chars(line, start.1, end.1.saturating_add(1));
        }

        let mut out = slice_chars(&self.lines[start.0], start.1, usize::MAX);
        for row in (start.0 + 1)..last_row {
            out.push('\n');
            out.push_str(&self.lines[row]);
        }
        out.push('\n');
        out.push_str(&slice_chars(&self.lines[last_row], 0, end.1.saturating_add(1)));
        out
    }

    /// Removes an **inclusive** character range, joining what is left of the
    /// first and last lines. Returns `true` if anything was removed.
    pub fn delete_range(&mut self, start: (usize, usize), end: (usize, usize)) -> bool {
        let (start, end) = order(start, end);
        if start.0 >= self.lines.len() {
            return false;
        }
        let last_row = end.0.min(self.lines.len().saturating_sub(1));

        if start.0 == last_row {
            let line = &self.lines[start.0];
            let head = slice_chars(line, 0, start.1);
            let tail = slice_chars(line, end.1.saturating_add(1), usize::MAX);
            self.lines[start.0] = head + &tail;
            return true;
        }

        let head = slice_chars(&self.lines[start.0], 0, start.1);
        let tail = slice_chars(&self.lines[last_row], end.1.saturating_add(1), usize::MAX);
        self.lines.drain(start.0..=last_row);
        self.lines.insert(start.0, head + &tail);
        true
    }

    /// Inserts text at a character position, splitting the line on newlines.
    /// Returns where the cursor should land: the last character written.
    pub fn insert_text(&mut self, row: usize, col: usize, text: &str) -> (usize, usize) {
        if row >= self.lines.len() || text.is_empty() {
            return (row, col);
        }

        let mut cursor = (row, col);
        for (index, chunk) in text.split('\n').enumerate() {
            if index > 0 {
                self.insert_newline(cursor.0, cursor.1);
                cursor = (cursor.0 + 1, 0);
            }
            for ch in chunk.chars() {
                self.insert_char(cursor.0, cursor.1, ch);
                cursor.1 += 1;
            }
        }
        (cursor.0, cursor.1.saturating_sub(1))
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

/// `[from, to)` in characters, clamped at both ends. Character indices again,
/// not bytes: slicing `café` by byte is how an editor corrupts a file.
fn slice_chars(line: &str, from: usize, to: usize) -> String {
    let start = char_to_byte(line, from);
    let end = char_to_byte(line, to.max(from));
    line[start..end.max(start)].to_string()
}

/// Puts two positions in document order, so callers never have to care which
/// end of a selection the user started from.
fn order(a: (usize, usize), b: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    if (a.0, a.1) <= (b.0, b.1) { (a, b) } else { (b, a) }
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
    fn replaces_a_whole_line() {
        let mut buffer = Buffer::from_text("uno\ndos");
        assert!(buffer.replace_line(1, "DOS".into()));
        assert_eq!(buffer.to_text(), "uno\nDOS");
        assert!(!buffer.replace_line(9, "nada".into()));
    }

    // ── Character ranges ────────────────────────────────────────

    #[test]
    fn reads_a_range_inside_one_line() {
        let buffer = Buffer::from_text("hello world");
        assert_eq!(buffer.range_text((0, 0), (0, 4)), "hello");
        assert_eq!(buffer.range_text((0, 6), (0, 10)), "world");
    }

    #[test]
    fn a_range_reads_the_same_backwards() {
        // The user may have selected right-to-left; the range is the range.
        let buffer = Buffer::from_text("hello");
        assert_eq!(buffer.range_text((0, 4), (0, 1)), buffer.range_text((0, 1), (0, 4)));
    }

    #[test]
    fn reads_a_range_across_lines() {
        let buffer = Buffer::from_text("uno\ndos\ntres");
        // Inclusive at both ends: "uno" from 1, all of "dos", "tres" up to and
        // including index 1.
        assert_eq!(buffer.range_text((0, 1), (2, 1)), "no\ndos\ntr");
    }

    #[test]
    fn a_range_is_inclusive_of_the_character_under_the_cursor() {
        let buffer = Buffer::from_text("abc");
        assert_eq!(buffer.range_text((0, 1), (0, 1)), "b");
    }

    #[test]
    fn deletes_a_range_inside_one_line() {
        let mut buffer = Buffer::from_text("hello world");
        assert!(buffer.delete_range((0, 5), (0, 10)));
        assert_eq!(buffer.line(0), Some("hello"));
    }

    #[test]
    fn deleting_across_lines_joins_what_is_left() {
        let mut buffer = Buffer::from_text("uno\ndos\ntres");
        assert!(buffer.delete_range((0, 1), (2, 1)));
        assert_eq!(buffer.to_text(), "ues");
        assert_eq!(buffer.line_count(), 1);
    }

    #[test]
    fn range_operations_respect_multibyte_characters() {
        let buffer = Buffer::from_text("café con leche");
        assert_eq!(buffer.range_text((0, 0), (0, 3)), "café");

        // Characters 4..=7 are " con" — the é before them is one character and
        // two bytes, which is exactly what a byte-indexed slice would get wrong.
        let mut buffer = Buffer::from_text("café con leche");
        assert!(buffer.delete_range((0, 4), (0, 7)));
        assert_eq!(buffer.line(0), Some("café leche"));
    }

    #[test]
    fn inserts_text_with_newlines_at_a_position() {
        let mut buffer = Buffer::from_text("ad");
        let landed = buffer.insert_text(0, 1, "b\nc");
        assert_eq!(buffer.to_text(), "ab\ncd");
        assert_eq!(landed, (1, 0));
    }

    #[test]
    fn deleting_last_line_keeps_buffer_usable() {
        let mut buffer = Buffer::from_text("only");
        assert!(buffer.delete_line(0));
        assert_eq!(buffer.line_count(), 1);
        assert_eq!(buffer.line(0), Some(""));
    }
}
