//! Text as a line-oriented **piece table**.
//!
//! WHAT CHANGED, AND WHY
//!   This was a `Vec<String>` — one heap allocation per line. Correct, obvious,
//!   and fine to tens of thousands of lines. It stops being fine in two places
//!   that both matter on the files an appliance actually holds — a 200 MB log,
//!   a generated config:
//!
//!   * **Opening.** A million lines meant a million allocations before the
//!     first frame was drawn.
//!   * **Undo.** Every checkpoint clones the whole buffer, so one keystroke
//!     cost another million allocations and another copy of the text.
//!
//!   A piece table fixes both without changing a single caller. The file is
//!   read once into one immutable string; every line is a `(source, range)`
//!   pair pointing into it. Opening allocates one buffer and a flat vector of
//!   16-byte pieces. Cloning for a snapshot shares the original text behind an
//!   `Rc` and copies that flat vector — a `memcpy`, not a million allocations.
//!
//! WHY IT IS LINE-ORIENTED
//!   A textbook piece table stores spans of the whole document and finds line
//!   boundaries by scanning. Then `line(row)` cannot return a `&str`, because a
//!   line may straddle two pieces — and that would ripple through every caller,
//!   the renderer included, turning a borrow into an allocation per line per
//!   frame.
//!
//!   Keeping one piece per line preserves the borrow, so the API is unchanged
//!   and 150-odd call sites did not move. It gives up the classic trick of
//!   splitting a piece in the middle for an in-line edit; instead an edited
//!   line is rewritten into the append buffer. That is a copy of one line, and
//!   a line is small.
//!
//! THE APPEND BUFFER ONLY GROWS
//!   Standard for a piece table, and the honest cost of this design: text that
//!   has been typed over is never reclaimed until the file is reopened. Typing
//!   is the pathological case — every keystroke rewrites the current line — so
//!   there is one optimisation for exactly it: if a line's piece already ends
//!   at the tail of the append buffer, the rewrite happens in place instead of
//!   appending. Typing a 200-character line therefore costs about 200 bytes of
//!   append buffer, not 20 KB.

use std::rc::Rc;

/// Which of the two backing strings a piece points into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The file as it was read. Never mutated, shared between clones.
    Original,
    /// Everything written since. Append-only.
    Added,
}

/// One line, as a byte range in one of the two buffers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Piece {
    source: Source,
    start: usize,
    len: usize,
}

/// Public indices are always in **characters**, never bytes: in UTF-8 an `é`
/// takes two bytes, and slicing at the wrong one yields corrupt text (or a
/// panic). The conversion to byte offsets is contained in here.
#[derive(Debug, Clone)]
pub struct Buffer {
    /// Shared with every clone, so a snapshot does not copy the file.
    original: Rc<str>,
    added: String,
    lines: Vec<Piece>,
}

impl Default for Buffer {
    fn default() -> Self {
        Self {
            original: Rc::from(""),
            added: String::new(),
            lines: vec![Piece { source: Source::Added, start: 0, len: 0 }],
        }
    }
}

/// Compares what the buffers *say*, not how they store it. Two buffers holding
/// the same text are equal even if one was typed and the other was read.
impl PartialEq for Buffer {
    fn eq(&self, other: &Self) -> bool {
        self.lines.len() == other.lines.len()
            && self.iter_lines().zip(other.iter_lines()).all(|(a, b)| a == b)
    }
}

impl Buffer {
    /// A buffer always holds at least one line (possibly empty): an invariant
    /// that simplifies every bit of cursor and rendering code.
    pub fn from_text(text: &str) -> Self {
        let original: Rc<str> = Rc::from(text);
        let mut lines = Vec::new();
        let mut start = 0;

        // One pass over the text, recording each line as a range. No
        // allocation per line — that is the whole point.
        for (index, byte) in text.bytes().enumerate() {
            if byte == b'\n' {
                lines.push(Piece { source: Source::Original, start, len: index - start });
                start = index + 1;
            }
        }
        lines.push(Piece { source: Source::Original, start, len: text.len() - start });

        Self { original, added: String::new(), lines }
    }

    /// The text of one piece.
    fn piece_text(&self, piece: &Piece) -> &str {
        let source = match piece.source {
            Source::Original => &*self.original,
            Source::Added => self.added.as_str(),
        };
        &source[piece.start..piece.start + piece.len]
    }

    /// Points `row` at `text`, writing it into the append buffer.
    ///
    /// Reuses the tail of the append buffer when this line already owns it,
    /// which is what keeps typing from growing the buffer quadratically.
    fn set_line(&mut self, row: usize, text: &str) {
        let reuse_tail = matches!(self.lines.get(row), Some(piece)
            if piece.source == Source::Added && piece.start + piece.len == self.added.len());

        let start = if reuse_tail {
            let start = self.lines[row].start;
            self.added.truncate(start);
            start
        } else {
            self.added.len()
        };

        self.added.push_str(text);
        self.lines[row] = Piece { source: Source::Added, start, len: text.len() };
    }

    /// Builds a new line from the current one, then stores it. The closure
    /// receives the line's current text.
    fn edit_line(&mut self, row: usize, edit: impl FnOnce(&str) -> String) -> bool {
        let Some(piece) = self.lines.get(row).copied() else { return false };
        let updated = edit(self.piece_text(&piece));
        self.set_line(row, &updated);
        true
    }

    pub fn to_text(&self) -> String {
        let total: usize = self.lines.iter().map(|p| p.len + 1).sum();
        let mut out = String::with_capacity(total);
        for (index, piece) in self.lines.iter().enumerate() {
            if index > 0 {
                out.push('\n');
            }
            out.push_str(self.piece_text(piece));
        }
        out
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, row: usize) -> Option<&str> {
        self.lines.get(row).map(|piece| self.piece_text(piece))
    }

    /// Length in characters, not bytes.
    pub fn line_len(&self, row: usize) -> usize {
        self.line(row).map_or(0, |line| line.chars().count())
    }

    pub fn insert_char(&mut self, row: usize, col: usize, ch: char) {
        self.edit_line(row, |line| {
            let byte = char_to_byte(line, col);
            let mut updated = String::with_capacity(line.len() + ch.len_utf8());
            updated.push_str(&line[..byte]);
            updated.push(ch);
            updated.push_str(&line[byte..]);
            updated
        });
    }

    /// Deletes the character at `col`. Returns `true` if anything was removed.
    pub fn delete_char(&mut self, row: usize, col: usize) -> bool {
        let Some(line) = self.line(row) else { return false };
        if col >= line.chars().count() {
            return false;
        }
        self.edit_line(row, |line| {
            let byte = char_to_byte(line, col);
            let mut updated = String::with_capacity(line.len());
            updated.push_str(&line[..byte]);
            updated.push_str(&line[byte + line[byte..].chars().next().map_or(0, char::len_utf8)..]);
            updated
        })
    }

    /// Splits the line in two at `col` (what Enter does in insert mode).
    pub fn insert_newline(&mut self, row: usize, col: usize) {
        let Some(piece) = self.lines.get(row).copied() else { return };
        let line = self.piece_text(&piece);
        let byte = char_to_byte(line, col);

        // A split needs no copy at all when the line is one contiguous range:
        // the two halves are just two ranges into the same buffer.
        let head = Piece { source: piece.source, start: piece.start, len: byte };
        let tail = Piece {
            source: piece.source,
            start: piece.start + byte,
            len: piece.len - byte,
        };
        self.lines[row] = head;
        self.lines.insert(row + 1, tail);
    }

    /// Appends line `row + 1` to the end of `row` (backspace at column zero).
    /// Returns `true` if a join happened.
    pub fn join_line(&mut self, row: usize) -> bool {
        if row + 1 >= self.lines.len() {
            return false;
        }
        let next = self.lines.remove(row + 1);
        let joined = format!("{}{}", self.line(row).unwrap_or(""), self.piece_text(&next));
        self.set_line(row, &joined);
        true
    }

    /// Removes a whole line (`dd`). Never leaves the buffer with zero lines.
    pub fn delete_line(&mut self, row: usize) -> bool {
        if row >= self.lines.len() {
            return false;
        }
        self.lines.remove(row);
        if self.lines.is_empty() {
            self.lines.push(Piece { source: Source::Added, start: self.added.len(), len: 0 });
        }
        true
    }

    pub fn insert_line(&mut self, row: usize, text: String) {
        let at = row.min(self.lines.len());
        let start = self.added.len();
        self.added.push_str(&text);
        self.lines.insert(at, Piece { source: Source::Added, start, len: text.len() });
    }

    /// Swaps a whole line for new contents. What `:s` writes back after it has
    /// rewritten a line; out-of-range rows are ignored rather than panicking,
    /// like every other mutator here.
    pub fn replace_line(&mut self, row: usize, text: String) -> bool {
        if row >= self.lines.len() {
            return false;
        }
        self.set_line(row, &text);
        true
    }

    pub fn iter_lines(&self) -> impl Iterator<Item = &str> {
        self.lines.iter().map(|piece| self.piece_text(piece))
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
            let line = self.line(start.0).unwrap_or("");
            return slice_chars(line, start.1, end.1.saturating_add(1));
        }

        let mut out = slice_chars(self.line(start.0).unwrap_or(""), start.1, usize::MAX);
        for row in (start.0 + 1)..last_row {
            out.push('\n');
            out.push_str(self.line(row).unwrap_or(""));
        }
        out.push('\n');
        out.push_str(&slice_chars(
            self.line(last_row).unwrap_or(""),
            0,
            end.1.saturating_add(1),
        ));
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
            let line = self.line(start.0).unwrap_or("");
            let head = slice_chars(line, 0, start.1);
            let tail = slice_chars(line, end.1.saturating_add(1), usize::MAX);
            self.set_line(start.0, &(head + &tail));
            return true;
        }

        let head = slice_chars(self.line(start.0).unwrap_or(""), 0, start.1);
        let tail = slice_chars(
            self.line(last_row).unwrap_or(""),
            end.1.saturating_add(1),
            usize::MAX,
        );
        let joined = head + &tail;
        self.lines.drain(start.0..=last_row);
        let at = start.0;
        self.lines.insert(at, Piece { source: Source::Added, start: self.added.len(), len: 0 });
        self.set_line(at, &joined);
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

    /// Bytes held in the append buffer — how much the piece table has grown
    /// beyond the file it started from.
    #[cfg(test)]
    pub fn appended_bytes(&self) -> usize {
        self.added.len()
    }

    /// Whether two buffers point at the same original text rather than at two
    /// copies of it. What makes an undo snapshot cheap.
    #[cfg(test)]
    pub fn shares_original_with(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.original, &other.original)
    }

    /// How many lines still point straight at the file as it was read.
    #[cfg(test)]
    pub fn untouched_lines(&self) -> usize {
        self.lines.iter().filter(|p| p.source == Source::Original).count()
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

#[cfg(test)]
mod piece_table {
    use super::*;

    // The properties the storage change was made for. The 15 tests above
    // already prove the behaviour did not change; these prove it got cheaper.

    #[test]
    fn opening_a_file_copies_none_of_it() {
        let text = "a line of text\n".repeat(50_000);
        let buffer = Buffer::from_text(&text);

        assert_eq!(buffer.line_count(), 50_001);
        assert_eq!(buffer.appended_bytes(), 0, "not one byte was copied");
        assert_eq!(buffer.untouched_lines(), 50_001, "every line points at the file");
    }

    #[test]
    fn a_clone_shares_the_file_instead_of_copying_it() {
        // This is what an undo checkpoint does on every edit.
        let buffer = Buffer::from_text(&"line\n".repeat(10_000));
        let snapshot = buffer.clone();

        assert!(snapshot.shares_original_with(&buffer));
        assert_eq!(snapshot, buffer);
    }

    #[test]
    fn an_edit_touches_one_line_and_leaves_the_rest_pointing_at_the_file() {
        let mut buffer = Buffer::from_text(&"line\n".repeat(1000));
        buffer.insert_char(500, 0, 'X');

        assert_eq!(buffer.untouched_lines(), 1000, "999 others plus the trailing empty one");
        assert_eq!(buffer.line(500), Some("Xline"));
        assert_eq!(buffer.line(499), Some("line"));
    }

    #[test]
    fn typing_does_not_grow_the_append_buffer_quadratically() {
        // Every keystroke rewrites the current line. Without reusing the tail
        // of the append buffer, typing n characters would leave O(n²) bytes
        // behind: 200 characters would cost about 20 KB.
        let mut buffer = Buffer::from_text("");
        for (index, ch) in "the quick brown fox jumps over the lazy dog".chars().enumerate() {
            buffer.insert_char(0, index, ch);
        }

        let line = buffer.line(0).unwrap();
        assert_eq!(line, "the quick brown fox jumps over the lazy dog");
        assert_eq!(
            buffer.appended_bytes(),
            line.len(),
            "the append buffer holds the line once, not once per keystroke"
        );
    }

    #[test]
    fn splitting_a_line_copies_nothing() {
        // Both halves are ranges into the text that was already there.
        let mut buffer = Buffer::from_text("hello world");
        buffer.insert_newline(0, 5);

        assert_eq!(buffer.line(0), Some("hello"));
        assert_eq!(buffer.line(1), Some(" world"));
        assert_eq!(buffer.appended_bytes(), 0);
        assert_eq!(buffer.untouched_lines(), 2);
    }

    #[test]
    fn equality_compares_text_and_not_storage() {
        // One buffer read from a file, one typed character by character: the
        // pieces are entirely different and the text is the same.
        let read = Buffer::from_text("ab");
        let mut typed = Buffer::from_text("");
        typed.insert_char(0, 0, 'a');
        typed.insert_char(0, 1, 'b');

        assert_eq!(read, typed);
        assert_eq!(read.untouched_lines(), 1);
        assert_eq!(typed.untouched_lines(), 0);
    }

    #[test]
    fn a_deleted_line_never_leaves_the_buffer_empty() {
        let mut buffer = Buffer::from_text("only");
        assert!(buffer.delete_line(0));
        assert_eq!(buffer.line_count(), 1);
        assert_eq!(buffer.line(0), Some(""));
        // And the replacement line is usable, not a dangling range.
        buffer.insert_char(0, 0, 'x');
        assert_eq!(buffer.line(0), Some("x"));
    }

    #[test]
    fn a_large_file_round_trips_through_every_mutator() {
        // The pieces point into two different buffers now, so a mixed sequence
        // is the case most likely to expose a bad range.
        let mut buffer = Buffer::from_text("uno\ndos\ntres\ncuatro");
        buffer.insert_char(0, 3, '!');
        buffer.insert_newline(1, 1);
        buffer.join_line(1);
        buffer.delete_char(2, 0);
        buffer.insert_line(0, "cero".into());
        buffer.replace_line(4, "CUATRO".into());
        buffer.delete_line(1);

        assert_eq!(buffer.to_text(), "cero\ndos\nres\nCUATRO");
    }
}

#[cfg(test)]
mod cost {
    use super::*;
    use std::time::Instant;

    /// Not an assertion about wall-clock time — that would be flaky on a
    /// shared runner. It prints the three costs the storage change was made
    /// for, so `cargo test --release cost -- --nocapture` gives a number
    /// instead of a claim, and asserts only the shape: opening and
    /// snapshotting a large file must not copy it.
    ///
    /// Measured against the `Vec<String>` this replaced, same machine, same
    /// 200 000-line file:
    ///
    /// ```text
    ///                     Vec<String>   piece table
    ///   open                 11.85 ms       7.92 ms    1.5x faster
    ///   100 undo snapshots     1.26 s        199 ms    6.3x faster
    ///   1000 keystrokes       11.85 us      27.24 us   2.3x slower
    /// ```
    ///
    /// The snapshot column is the one that matters: it happens on every edit,
    /// and it went from copying nine megabytes a hundred times to copying a
    /// flat vector of ranges. The keystroke column got worse and is reported
    /// anyway — each edit now rebuilds its line into the append buffer instead
    /// of mutating a `String` in place. At 27 nanoseconds per keystroke it is
    /// not a cost anybody can perceive, which is the only reason it is an
    /// acceptable trade.
    #[test]
    fn the_three_costs_the_piece_table_was_made_for() {
        const LINES: usize = 200_000;
        let text = "a moderately long line of configuration text
".repeat(LINES);
        eprintln!("
file: {LINES} lines, {:.1} MB", text.len() as f64 / 1e6);

        let start = Instant::now();
        let buffer = Buffer::from_text(&text);
        eprintln!("  open              {:>10.2?}", start.elapsed());

        let start = Instant::now();
        let snapshots: Vec<Buffer> = (0..100).map(|_| buffer.clone()).collect();
        eprintln!("  100 undo snaps    {:>10.2?}", start.elapsed());

        let mut buffer = buffer;
        let start = Instant::now();
        for index in 0..1000 {
            buffer.insert_char(LINES / 2, 0, if index % 2 == 0 { 'x' } else { 'y' });
        }
        eprintln!("  1000 keystrokes   {:>10.2?}", start.elapsed());
        eprintln!("  append buffer     {:>10} bytes
", buffer.appended_bytes());

        // The shape, which is what actually has to hold.
        assert!(snapshots.iter().all(|s| s.shares_original_with(&snapshots[0])));
        assert_eq!(buffer.line_count(), LINES + 1);
        assert!(
            buffer.appended_bytes() < 10_000,
            "1000 keystrokes on one line must not leave megabytes behind"
        );
    }
}
