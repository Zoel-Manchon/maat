//! Cursor position and motions.
//!
//! Design rule: the cursor can **never** end up outside the buffer. Rather
//! than validating at every call site that moves it, all motions go through
//! here and are clamped against the buffer. A state you cannot represent is a
//! state you don't have to test for.

use super::buffer::Buffer;
use super::mode::Mode;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

impl Cursor {
    /// In Normal the cursor sits *on* a character, so its maximum is
    /// `len - 1`. In Insert it sits *between* characters, so it may reach
    /// `len` (past the last one). That off-by-one is the source of half the
    /// strange bugs in modal editors.
    fn max_col(buffer: &Buffer, row: usize, mode: Mode) -> usize {
        let len = buffer.line_len(row);
        match mode {
            Mode::Insert => len,
            _ => len.saturating_sub(1),
        }
    }

    pub fn left(&mut self) {
        self.col = self.col.saturating_sub(1);
    }

    pub fn right(&mut self, buffer: &Buffer, mode: Mode) {
        let max = Self::max_col(buffer, self.row, mode);
        if self.col < max {
            self.col += 1;
        }
    }

    pub fn up(&mut self, buffer: &Buffer, mode: Mode) {
        if self.row > 0 {
            self.row -= 1;
            self.clamp_col(buffer, mode);
        }
    }

    pub fn down(&mut self, buffer: &Buffer, mode: Mode) {
        if self.row + 1 < buffer.line_count() {
            self.row += 1;
            self.clamp_col(buffer, mode);
        }
    }

    pub fn line_start(&mut self) {
        self.col = 0;
    }

    pub fn line_end(&mut self, buffer: &Buffer, mode: Mode) {
        self.col = Self::max_col(buffer, self.row, mode);
    }

    pub fn buffer_start(&mut self) {
        self.row = 0;
        self.col = 0;
    }

    pub fn buffer_end(&mut self, buffer: &Buffer, mode: Mode) {
        self.row = buffer.line_count().saturating_sub(1);
        self.clamp_col(buffer, mode);
    }

    /// Re-clamps the column after a line or mode change. Moving from a long
    /// line down to a short one must not leave the cursor floating.
    pub fn clamp_col(&mut self, buffer: &Buffer, mode: Mode) {
        self.col = self.col.min(Self::max_col(buffer, self.row, mode));
    }

    /// Re-clamps everything after an edit that may have shrunk the buffer.
    pub fn clamp(&mut self, buffer: &Buffer, mode: Mode) {
        self.row = self.row.min(buffer.line_count().saturating_sub(1));
        self.clamp_col(buffer, mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Buffer {
        Buffer::from_text("hello\nhow are you\ngoodbye")
    }

    #[test]
    fn does_not_move_past_buffer_edges() {
        let buffer = sample();
        let mut cursor = Cursor::default();

        cursor.left();
        cursor.up(&buffer, Mode::Normal);
        assert_eq!(cursor, Cursor { row: 0, col: 0 });

        cursor.buffer_end(&buffer, Mode::Normal);
        cursor.down(&buffer, Mode::Normal);
        assert_eq!(cursor.row, 2);
    }

    #[test]
    fn normal_mode_stops_on_last_char_insert_mode_goes_past_it() {
        let buffer = Buffer::from_text("abc");

        let mut normal = Cursor::default();
        normal.line_end(&buffer, Mode::Normal);
        assert_eq!(normal.col, 2);

        let mut insert = Cursor::default();
        insert.line_end(&buffer, Mode::Insert);
        assert_eq!(insert.col, 3);
    }

    #[test]
    fn clamps_column_when_moving_to_shorter_line() {
        let buffer = Buffer::from_text("a very long line\nshort");
        let mut cursor = Cursor { row: 0, col: 12 };

        cursor.down(&buffer, Mode::Normal);
        assert_eq!(cursor.row, 1);
        assert_eq!(cursor.col, 4); // "short" is 5 characters → max 4
    }

    #[test]
    fn clamp_recovers_from_a_shrunken_buffer() {
        let buffer = Buffer::from_text("one");
        let mut cursor = Cursor { row: 7, col: 40 };

        cursor.clamp(&buffer, Mode::Normal);
        assert_eq!(cursor, Cursor { row: 0, col: 2 });
    }
}
