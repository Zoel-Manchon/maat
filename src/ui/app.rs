//! Editor state and the translation of keys into actions.
//!
//! The event loop lives in `main.rs`; what each key *does* lives here. `App`
//! draws nothing, so its modal behaviour can be tested without standing up a
//! real terminal.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::collections::VecDeque;
use std::path::Path;

use crate::core::audit::{self, SaveEvent};
use crate::core::buffer::Buffer;
use crate::core::cursor::Cursor;
use crate::core::document::{DiskState, Document};
use crate::core::mode::Mode;

const HISTORY_LIMIT: usize = 512;

/// Severity of the message shown on the bottom line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
struct Snapshot {
    buffer: Buffer,
    cursor: Cursor,
}

pub struct App {
    pub document: Document,
    pub cursor: Cursor,
    pub mode: Mode,
    /// First visible line (vertical scroll).
    pub offset_row: usize,
    /// First visible column (horizontal scroll on long lines).
    pub offset_col: usize,
    /// What has been typed after `:` while in Command mode.
    pub command: String,
    /// The query being typed after `/`.
    pub search: String,
    /// Last accepted search; `n` and `N` reuse it.
    pub last_search: String,
    pub message: String,
    pub level: Level,
    pub show_help: bool,
    pub relative_numbers: bool,
    /// Pending key of a two-stroke command (`gg`, `dd`, `yy`).
    pending: Option<char>,
    /// Where an incremental search started from.
    search_origin: Cursor,
    /// Simple line register backing `yy`, `dd`, `p` and `P`.
    register: Option<String>,
    undo_stack: VecDeque<Snapshot>,
    redo_stack: VecDeque<Snapshot>,
    /// Cached SHA-256 of the buffer: recomputing it every frame would be
    /// costly on large files. Refreshed only after an edit or a history jump.
    hash_cache: String,
    pub quit: bool,
    /// True when the session ended via `:q!` with unsaved changes on the table.
    /// `visudo` and friends rely on a non-zero exit to know the edit was
    /// abandoned, so they don't install a half-written file.
    pub discarded: bool,
}

impl App {
    pub fn new(document: Document) -> Self {
        let hash_cache = document.buffer_hash();
        Self {
            document,
            cursor: Cursor::default(),
            mode: Mode::Normal,
            offset_row: 0,
            offset_col: 0,
            command: String::new(),
            search: String::new(),
            last_search: String::new(),
            message: String::new(),
            level: Level::Info,
            show_help: false,
            relative_numbers: false,
            pending: None,
            search_origin: Cursor::default(),
            register: None,
            undo_stack: VecDeque::new(),
            redo_stack: VecDeque::new(),
            hash_cache,
            quit: false,
            discarded: false,
        }
    }

    pub fn hash(&self) -> &str {
        &self.hash_cache
    }

    pub fn is_modified(&self) -> bool {
        self.document.is_hash_modified(&self.hash_cache)
    }

    pub fn active_search(&self) -> &str {
        if self.mode == Mode::Search {
            &self.search
        } else {
            &self.last_search
        }
    }

    pub fn is_welcome(&self) -> bool {
        self.document.path().is_none()
            && self.document.buffer.line_count() == 1
            && self.document.buffer.line(0) == Some("")
            && !self.is_modified()
            && self.mode == Mode::Normal
            && !self.show_help
    }

    pub fn stats(&self) -> (usize, usize, usize) {
        let lines = self.document.buffer.line_count();
        let words = self
            .document
            .buffer
            .iter_lines()
            .map(|line| line.split_whitespace().count())
            .sum();
        let chars_in_lines: usize = self
            .document
            .buffer
            .iter_lines()
            .map(|line| line.chars().count())
            .sum();
        let chars = chars_in_lines + lines.saturating_sub(1);
        (lines, words, chars)
    }

    /// The single point every edit flows through: re-clamps the cursor inside
    /// the buffer and refreshes the hash.
    fn after_edit(&mut self) {
        self.cursor.clamp(&self.document.buffer, self.mode);
        self.hash_cache = self.document.buffer_hash();
    }

    fn notify(&mut self, text: impl Into<String>, level: Level) {
        self.message = text.into();
        self.level = level;
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            buffer: self.document.buffer.clone(),
            cursor: self.cursor,
        }
    }

    /// Records an undo point immediately before a mutation.
    fn checkpoint(&mut self) {
        if self.undo_stack.len() == HISTORY_LIMIT {
            self.undo_stack.pop_front();
        }
        self.undo_stack.push_back(self.snapshot());
        self.redo_stack.clear();
    }

    fn undo(&mut self) {
        let Some(previous) = self.undo_stack.pop_back() else {
            self.notify("nothing to undo", Level::Info);
            return;
        };

        self.redo_stack.push_back(self.snapshot());
        self.document.buffer = previous.buffer;
        self.cursor = previous.cursor;
        self.mode = Mode::Normal;
        self.after_edit();
        self.notify("change undone", Level::Info);
    }

    fn redo(&mut self) {
        let Some(next) = self.redo_stack.pop_back() else {
            self.notify("nothing to redo", Level::Info);
            return;
        };

        self.undo_stack.push_back(self.snapshot());
        self.document.buffer = next.buffer;
        self.cursor = next.cursor;
        self.mode = Mode::Normal;
        self.after_edit();
        self.notify("change redone", Level::Info);
    }

    /// Adjusts the scroll so the cursor always stays in view.
    pub fn ensure_visible(&mut self, height: usize, width: usize) {
        if height == 0 || width == 0 {
            return;
        }
        if self.cursor.row < self.offset_row {
            self.offset_row = self.cursor.row;
        } else if self.cursor.row >= self.offset_row + height {
            self.offset_row = self.cursor.row - height + 1;
        }

        if self.cursor.col < self.offset_col {
            self.offset_col = self.cursor.col;
        } else if self.cursor.col >= self.offset_col + width {
            self.offset_col = self.cursor.col - width + 1;
        }
    }

    // ── Input ───────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent) {
        if self.show_help {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')) {
                self.show_help = false;
            }
            return;
        }

        // Quick save, available in any mode. Some terminals swallow Ctrl-S;
        // `:w` always remains the canonical route.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.write(false);
            return;
        }

        match self.mode {
            Mode::Normal => self.handle_normal(key),
            Mode::Insert => self.handle_insert(key),
            Mode::Command => self.handle_command(key),
            Mode::Search => self.handle_search(key),
        }
    }

    fn handle_normal(&mut self, key: KeyEvent) {
        // Two-stroke commands: if one is pending, this key completes it.
        if let Some(first) = self.pending.take() {
            match (first, key.code) {
                ('g', KeyCode::Char('g')) => {
                    self.cursor.buffer_start();
                    return;
                }
                ('d', KeyCode::Char('d')) => {
                    if let Some(line) = self.document.buffer.line(self.cursor.row) {
                        self.register = Some(line.to_string());
                        self.checkpoint();
                        self.document.buffer.delete_line(self.cursor.row);
                        self.after_edit();
                        self.notify("line deleted and yanked", Level::Info);
                    }
                    return;
                }
                ('y', KeyCode::Char('y')) => {
                    if let Some(line) = self.document.buffer.line(self.cursor.row) {
                        self.register = Some(line.to_string());
                        self.notify("line yanked", Level::Info);
                    }
                    return;
                }
                _ => {} // sequence aborted: this key is processed as a normal one
            }
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.redo();
            return;
        }

        match key.code {
            // Motion
            KeyCode::Char('h') | KeyCode::Left => self.cursor.left(),
            KeyCode::Char('l') | KeyCode::Right => self.cursor.right(&self.document.buffer, self.mode),
            KeyCode::Char('k') | KeyCode::Up => self.cursor.up(&self.document.buffer, self.mode),
            KeyCode::Char('j') | KeyCode::Down => self.cursor.down(&self.document.buffer, self.mode),
            KeyCode::Char('0') | KeyCode::Home => self.cursor.line_start(),
            KeyCode::Char('$') | KeyCode::End => self.cursor.line_end(&self.document.buffer, self.mode),
            KeyCode::Char('G') => self.cursor.buffer_end(&self.document.buffer, self.mode),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('w') => self.word_forward(),
            KeyCode::Char('b') => self.word_back(),

            // Entering insert mode
            KeyCode::Char('i') => self.enter_insert(),
            KeyCode::Char('a') => {
                self.enter_insert();
                self.cursor.right(&self.document.buffer, Mode::Insert);
            }
            KeyCode::Char('I') => {
                self.enter_insert();
                self.cursor.line_start();
            }
            KeyCode::Char('A') => {
                self.enter_insert();
                self.cursor.line_end(&self.document.buffer, Mode::Insert);
            }
            KeyCode::Char('o') => {
                self.checkpoint();
                self.document.buffer.insert_line(self.cursor.row + 1, String::new());
                self.cursor.row += 1;
                self.cursor.col = 0;
                self.enter_insert();
                self.after_edit();
            }
            KeyCode::Char('O') => {
                self.checkpoint();
                self.document.buffer.insert_line(self.cursor.row, String::new());
                self.cursor.col = 0;
                self.enter_insert();
                self.after_edit();
            }

            // Editing / register
            KeyCode::Char('x') => {
                if self.cursor.col < self.document.buffer.line_len(self.cursor.row) {
                    self.checkpoint();
                    self.document.buffer.delete_char(self.cursor.row, self.cursor.col);
                    self.after_edit();
                }
            }
            KeyCode::Char('d') => self.pending = Some('d'),
            KeyCode::Char('y') => self.pending = Some('y'),
            KeyCode::Char('p') => self.paste_line(false),
            KeyCode::Char('P') => self.paste_line(true),
            KeyCode::Char('u') => self.undo(),

            // Search and help
            KeyCode::Char('/') => self.enter_search(),
            KeyCode::Char('n') => self.repeat_search(false),
            KeyCode::Char('N') => self.repeat_search(true),
            KeyCode::Char('?') => self.show_help = true,

            // Command mode
            KeyCode::Char(':') => {
                self.mode = Mode::Command;
                self.command.clear();
                self.message.clear();
            }
            _ => {}
        }
    }

    fn handle_insert(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                // Leaving Insert steps the cursor back one: in Normal it sits
                // *on* a character, not past the last one.
                self.cursor.left();
                self.cursor.clamp(&self.document.buffer, self.mode);
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.checkpoint();
                self.document.buffer.insert_char(self.cursor.row, self.cursor.col, ch);
                self.cursor.col += 1;
                self.after_edit();
            }
            KeyCode::Enter => {
                self.checkpoint();
                self.document.buffer.insert_newline(self.cursor.row, self.cursor.col);
                self.cursor.row += 1;
                self.cursor.col = 0;
                self.after_edit();
            }
            KeyCode::Backspace => {
                if self.cursor.col > 0 {
                    self.checkpoint();
                    self.cursor.col -= 1;
                    self.document.buffer.delete_char(self.cursor.row, self.cursor.col);
                    self.after_edit();
                } else if self.cursor.row > 0 {
                    self.checkpoint();
                    let previous = self.cursor.row - 1;
                    let join_at = self.document.buffer.line_len(previous);
                    if self.document.buffer.join_line(previous) {
                        self.cursor.row = previous;
                        self.cursor.col = join_at;
                    }
                    self.after_edit();
                }
            }
            KeyCode::Left => self.cursor.left(),
            KeyCode::Right => self.cursor.right(&self.document.buffer, self.mode),
            KeyCode::Up => self.cursor.up(&self.document.buffer, self.mode),
            KeyCode::Down => self.cursor.down(&self.document.buffer, self.mode),
            _ => {}
        }
    }

    fn handle_command(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.command.clear();
            }
            KeyCode::Enter => {
                let command = std::mem::take(&mut self.command);
                self.mode = Mode::Normal;
                self.run_command(&command);
            }
            KeyCode::Backspace => {
                if self.command.pop().is_none() {
                    self.mode = Mode::Normal;
                }
            }
            KeyCode::Char(ch) => self.command.push(ch),
            _ => {}
        }
    }

    fn handle_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.cursor = self.search_origin;
                self.mode = Mode::Normal;
                self.search.clear();
                self.message.clear();
            }
            KeyCode::Enter => {
                if !self.search.is_empty() {
                    self.last_search = self.search.clone();
                    self.notify(format!("/{}", self.last_search), Level::Info);
                }
                self.search.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                if self.search.pop().is_none() {
                    self.cursor = self.search_origin;
                    self.mode = Mode::Normal;
                } else {
                    self.refresh_incremental_search();
                }
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.push(ch);
                self.refresh_incremental_search();
            }
            _ => {}
        }
    }

    fn enter_insert(&mut self) {
        self.mode = Mode::Insert;
        self.message.clear();
    }

    fn enter_search(&mut self) {
        self.mode = Mode::Search;
        self.search_origin = self.cursor;
        self.search.clear();
        self.message.clear();
    }

    fn paste_line(&mut self, above: bool) {
        let Some(text) = self.register.clone() else {
            self.notify("register empty — use yy or dd first", Level::Warn);
            return;
        };

        self.checkpoint();
        let row = if above { self.cursor.row } else { self.cursor.row + 1 };
        self.document.buffer.insert_line(row, text);
        self.cursor.row = row;
        self.cursor.col = 0;
        self.after_edit();
        self.notify("line pasted", Level::Info);
    }

    // ── Commands ────────────────────────────────────────────────

    fn run_command(&mut self, command: &str) {
        let command = command.trim();
        match command {
            "w" => {
                self.write(false);
            }
            "w!" => {
                self.write(true);
            }
            "q" => self.quit(false),
            "q!" => self.quit(true),
            "wq" | "x" => {
                if self.write(false) {
                    self.quit(true);
                }
            }
            "wq!" => {
                if self.write(true) {
                    self.quit(true);
                }
            }
            "hash" => {
                let hash = self.hash_cache.clone();
                self.notify(format!("sha256 {hash}"), Level::Info);
            }
            "check" => self.check_disk(),
            "info" => self.show_info(),
            "help" | "h" => self.show_help = true,
            "set relativenumber" | "set rnu" => {
                self.relative_numbers = true;
                self.notify("relative line numbers on", Level::Info);
            }
            "set number" | "set nornu" => {
                self.relative_numbers = false;
                self.notify("absolute line numbers on", Level::Info);
            }
            "" => {}
            other => {
                if let Some(path) = other.strip_prefix("w ") {
                    let path = path.trim().to_string();
                    match self.document.save_as(Path::new(&path)) {
                        Ok(()) => {
                            self.hash_cache = self.document.buffer_hash();
                            self.notify(format!("\"{path}\" written"), Level::Info);
                        }
                        Err(error) => {
                            self.notify(format!("could not write: {error}"), Level::Error);
                        }
                    }
                } else {
                    self.notify(format!("unknown command: {other}"), Level::Error);
                }
            }
        }
    }

    fn show_info(&mut self) {
        let (lines, words, chars) = self.stats();
        let path = self
            .document
            .path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "[sin ruta]".to_string());
        self.notify(
            format!("{path} · {lines} lines · {words} words · {chars} chars"),
            Level::Info,
        );
    }

    fn check_disk(&mut self) {
        match self.document.disk_state() {
            DiskState::Unchanged => self.notify("integrity OK — file unchanged on disk", Level::Info),
            DiskState::ModifiedExternally => self.notify(
                "alert — the file was modified externally",
                Level::Warn,
            ),
            DiskState::Missing => self.notify("alert — the file no longer exists on disk", Level::Warn),
            DiskState::NoFile => self.notify("new buffer — not on disk yet", Level::Info),
        }
    }

    /// Save, checking the on-disk integrity of the file first.
    fn write(&mut self, force: bool) -> bool {
        if !force {
            match self.document.disk_state() {
                DiskState::ModifiedExternally => {
                    let hash: String = self.hash_cache.chars().take(12).collect();
                    self.notify(
                        format!(
                            "⚠ file changed on disk (your buffer: {hash}…) — :w! to overwrite"
                        ),
                        Level::Warn,
                    );
                    return false;
                }
                DiskState::Missing => {
                    self.notify(
                        "⚠ the file has vanished from disk — :w! to recreate it",
                        Level::Warn,
                    );
                    return false;
                }
                _ => {}
            }
        }

        // Captured before the write: afterwards the document re-anchors it.
        let hash_before = self.document.disk_hash().map(str::to_owned);

        match self.document.save() {
            Ok(()) => {
                self.hash_cache = self.document.buffer_hash();
                self.emit_audit(hash_before.as_deref());
                let name = self.document.name();
                let lines = self.document.buffer.line_count();
                self.notify(format!("\"{name}\" {lines}L written"), Level::Info);
                true
            }
            Err(error) => {
                self.notify(format!("could not write: {error}"), Level::Error);
                false
            }
        }
    }

    /// Emits one audit line per save when `MAAT_AUDIT_LOG` is set.
    /// Failures are swallowed on purpose: an unwritable log must never cost
    /// the user their edit.
    fn emit_audit(&self, hash_before: Option<&str>) {
        let Some(path) = self.document.path() else { return };
        let event = SaveEvent {
            path,
            hash_before,
            hash_after: &self.hash_cache,
            lines: self.document.buffer.line_count(),
        };
        let _ = audit::log(&event);
    }

    fn quit(&mut self, force: bool) {
        if !force && self.is_modified() {
            self.notify(
                "unsaved changes — :w to write, :q! to discard",
                Level::Warn,
            );
            return;
        }
        // Forcing a quit over unsaved work is what `visudo` needs to see as a
        // non-zero exit: the edit was abandoned, don't install it.
        self.discarded = force && self.is_modified();
        self.quit = true;
    }

    // ── Search ──────────────────────────────────────────────────

    fn search_matches(&self, query: &str) -> Vec<Cursor> {
        if query.is_empty() {
            return Vec::new();
        }

        self.document
            .buffer
            .iter_lines()
            .enumerate()
            .flat_map(|(row, line)| {
                line.match_indices(query).map(move |(byte, _)| Cursor {
                    row,
                    col: line[..byte].chars().count(),
                })
            })
            .collect()
    }

    fn refresh_incremental_search(&mut self) {
        if self.search.is_empty() {
            self.cursor = self.search_origin;
            self.message.clear();
            return;
        }

        let matches = self.search_matches(&self.search);
        let found = matches.iter().copied().find(|position| {
            position.row > self.search_origin.row
                || (position.row == self.search_origin.row
                    && position.col >= self.search_origin.col)
        });

        if let Some(position) = found.or_else(|| matches.first().copied()) {
            self.cursor = position;
            self.level = Level::Info;
            self.message.clear();
        } else {
            self.cursor = self.search_origin;
            self.notify(format!("no matches: {}", self.search), Level::Warn);
        }
    }

    fn repeat_search(&mut self, reverse: bool) {
        if self.last_search.is_empty() {
            self.notify("no previous search", Level::Warn);
            return;
        }

        let matches = self.search_matches(&self.last_search);
        if matches.is_empty() {
            self.notify(format!("no matches: {}", self.last_search), Level::Warn);
            return;
        }

        let position = if reverse {
            matches
                .iter()
                .rev()
                .copied()
                .find(|position| {
                    position.row < self.cursor.row
                        || (position.row == self.cursor.row && position.col < self.cursor.col)
                })
                .or_else(|| matches.last().copied())
        } else {
            matches
                .iter()
                .copied()
                .find(|position| {
                    position.row > self.cursor.row
                        || (position.row == self.cursor.row && position.col > self.cursor.col)
                })
                .or_else(|| matches.first().copied())
        };

        if let Some(position) = position {
            self.cursor = position;
            self.notify(format!("/{}", self.last_search), Level::Info);
        }
    }

    // ── Word motions ────────────────────────────────────────────

    fn word_forward(&mut self) {
        let Some(line) = self.document.buffer.line(self.cursor.row) else { return };
        let chars: Vec<char> = line.chars().collect();
        let mut index = self.cursor.col;

        while index < chars.len() && !chars[index].is_whitespace() {
            index += 1;
        }
        while index < chars.len() && chars[index].is_whitespace() {
            index += 1;
        }

        if index >= chars.len() && self.cursor.row + 1 < self.document.buffer.line_count() {
            self.cursor.row += 1;
            self.cursor.col = 0;
        } else {
            self.cursor.col = index.min(chars.len().saturating_sub(1));
        }
    }

    fn word_back(&mut self) {
        if self.cursor.col == 0 {
            if self.cursor.row > 0 {
                self.cursor.row -= 1;
                self.cursor.line_end(&self.document.buffer, self.mode);
            }
            return;
        }

        let Some(line) = self.document.buffer.line(self.cursor.row) else { return };
        let chars: Vec<char> = line.chars().collect();
        let mut index = self.cursor.col - 1;

        while index > 0 && chars[index].is_whitespace() {
            index -= 1;
        }
        while index > 0 && !chars[index - 1].is_whitespace() {
            index -= 1;
        }
        self.cursor.col = index;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with(text: &str) -> App {
        App::new(Document::from_text_for_test(text))
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn hjkl_moves_the_cursor() {
        let mut app = app_with("abc\ndef");
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.cursor, Cursor { row: 1, col: 1 });

        press(&mut app, KeyCode::Char('h'));
        press(&mut app, KeyCode::Char('k'));
        assert_eq!(app.cursor, Cursor { row: 0, col: 0 });
    }

    #[test]
    fn i_enters_insert_and_esc_returns_to_normal() {
        let mut app = app_with("hello");
        press(&mut app, KeyCode::Char('i'));
        assert_eq!(app.mode, Mode::Insert);

        press(&mut app, KeyCode::Char('X'));
        assert_eq!(app.document.buffer.line(0), Some("Xhello"));

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn dd_deletes_and_yanks_the_current_line() {
        let mut app = app_with("one\ntwo\nthree");
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.document.buffer.to_text(), "one\nthree");

        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.document.buffer.to_text(), "one\nthree\ntwo");
    }

    #[test]
    fn undo_and_redo_restore_edits() {
        let mut app = app_with("abc");
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.document.buffer.to_text(), "bc");

        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.document.buffer.to_text(), "abc");

        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(app.document.buffer.to_text(), "bc");
    }

    #[test]
    fn search_moves_and_wraps() {
        let mut app = app_with("one two\nthree one");
        press(&mut app, KeyCode::Char('/'));
        for ch in "one".chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.cursor, Cursor { row: 0, col: 0 });

        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.cursor, Cursor { row: 1, col: 6 });
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.cursor, Cursor { row: 0, col: 0 });
    }

    #[test]
    fn gg_and_g_jump_to_the_edges() {
        let mut app = app_with("a\nb\nc");
        press(&mut app, KeyCode::Char('G'));
        assert_eq!(app.cursor.row, 2);

        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('g'));
        assert_eq!(app.cursor.row, 0);
    }

    #[test]
    fn enter_splits_the_line_in_insert_mode() {
        let mut app = app_with("helloworld");
        press(&mut app, KeyCode::Char('i'));
        for _ in 0..5 {
            press(&mut app, KeyCode::Right);
        }
        press(&mut app, KeyCode::Enter);

        assert_eq!(app.document.buffer.to_text(), "hello\nworld");
        assert_eq!(app.cursor, Cursor { row: 1, col: 0 });
    }

    #[test]
    fn backspace_at_line_start_joins_with_the_previous_line() {
        let mut app = app_with("hello\nworld");
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Backspace);

        assert_eq!(app.document.buffer.to_text(), "helloworld");
        assert_eq!(app.cursor, Cursor { row: 0, col: 5 });
    }

    #[test]
    fn quitting_with_unsaved_changes_warns_instead_of_exiting() {
        let mut app = app_with("something");
        press(&mut app, KeyCode::Char('x'));
        app.run_command("q");

        assert!(!app.quit);
        assert_eq!(app.level, Level::Warn);

        app.run_command("q!");
        assert!(app.quit);
    }

    #[test]
    fn the_hash_changes_when_the_buffer_changes() {
        let mut app = app_with("abc");
        let before = app.hash().to_string();

        press(&mut app, KeyCode::Char('x'));
        assert_ne!(app.hash(), before);
    }

    #[test]
    fn forced_write_and_quit_does_not_exit_when_write_fails() {
        let mut app = app_with("content");
        app.run_command("wq!");

        assert!(!app.quit);
        assert_eq!(app.level, Level::Error);
    }

    #[test]
    fn discarding_unsaved_changes_flags_the_session() {
        let mut app = app_with("draft");
        press(&mut app, KeyCode::Char('x'));

        app.run_command("q!");
        assert!(app.quit);
        assert!(app.discarded, "an abandoned edit must be visible to the caller");
    }

    #[test]
    fn a_clean_quit_is_not_flagged_as_discarded() {
        let mut app = app_with("draft");
        app.run_command("q!");

        assert!(app.quit);
        assert!(!app.discarded);
    }

    #[test]
    fn viewport_follows_the_cursor() {
        let text: String = (0..100).map(|n| format!("line {n}\n")).collect();
        let mut app = app_with(&text);

        app.cursor.row = 50;
        app.ensure_visible(10, 40);
        assert!(app.offset_row <= 50 && 50 < app.offset_row + 10);

        app.cursor.row = 3;
        app.ensure_visible(10, 40);
        assert_eq!(app.offset_row, 3);
    }
}
