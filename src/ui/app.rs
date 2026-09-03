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

/// What the register holds, and therefore how `p` puts it back.
///
/// Vim's distinction, and it matters: `yy` on one line and `vwy` over a word
/// both fill the register, but pasting the first opens a new line and pasting
/// the second drops the text in beside the cursor. Collapsing the two would
/// make every paste after a visual yank land in the wrong place.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Register {
    /// Whole lines, from `yy` / `dd` / `V`.
    Lines(Vec<String>),
    /// A character span, from a character-wise visual yank or delete.
    Chars(String),
}

/// A parsed `:s` command.
///
/// Matching is **literal**, not regular-expression based, which is the same
/// contract `/` already has. That is a deliberate limit rather than a missing
/// feature: the files this editor is aimed at — an appliance's sshd_config, a
/// sudoers file, a unit file — are edited under pressure, and a pattern that
/// quietly means something other than what it looks like is the last thing
/// that situation needs. What you type is what gets replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Substitution {
    pattern: String,
    replacement: String,
    /// `g`: every occurrence on a line, not just the first.
    global: bool,
    /// `%`: every line, not just the one under the cursor.
    whole_file: bool,
}

/// Parses `[%]s/pattern/replacement[/flags]`.
///
/// The character right after `s` is the delimiter, so `:s#/usr/bin#/bin#`
/// works without escaping every slash in a path — which is most of what one
/// substitutes in a config file. Inside the fields, the delimiter can still be
/// escaped with a backslash.
fn parse_substitution(command: &str) -> Option<Substitution> {
    let (whole_file, rest) = match command.strip_prefix('%') {
        Some(rest) => (true, rest),
        None => (false, command),
    };

    let rest = rest.strip_prefix('s')?;
    let mut chars = rest.chars();
    let delimiter = chars.next()?;
    if delimiter.is_alphanumeric() || delimiter.is_whitespace() {
        return None;
    }

    let mut fields: Vec<String> = vec![String::new()];
    let mut escaped = false;
    for ch in chars {
        if escaped {
            // A backslash only escapes the delimiter; anywhere else it stays a
            // literal backslash, because these files are full of them.
            if ch != delimiter {
                fields.last_mut()?.push('\\');
            }
            fields.last_mut()?.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == delimiter {
            fields.push(String::new());
        } else {
            fields.last_mut()?.push(ch);
        }
    }
    if escaped {
        fields.last_mut()?.push('\\');
    }

    let pattern = fields.first()?.clone();
    if pattern.is_empty() {
        return None;
    }
    let replacement = fields.get(1).cloned().unwrap_or_default();
    let flags = fields.get(2).map(String::as_str).unwrap_or("");
    if flags.chars().any(|flag| flag != 'g') {
        return None;
    }

    Some(Substitution {
        pattern,
        replacement,
        global: flags.contains('g'),
        whole_file,
    })
}

/// "1 line yanked" / "3 lines yanked" — singular and plural, so the status bar
/// never reads like a placeholder.
fn count_message(lines: usize, what: &str) -> String {
    let noun = if lines == 1 { "line" } else { "lines" };
    format!("{lines} {noun} {what}")
}

/// Applies a substitution to one line, returning the new line and how many
/// occurrences were replaced.
fn substitute_line(line: &str, rule: &Substitution) -> (String, usize) {
    if rule.global {
        let count = line.matches(&rule.pattern).count();
        (line.replace(&rule.pattern, &rule.replacement), count)
    } else {
        match line.find(&rule.pattern) {
            Some(at) => {
                let mut out = String::with_capacity(line.len());
                out.push_str(&line[..at]);
                out.push_str(&rule.replacement);
                out.push_str(&line[at + rule.pattern.len()..]);
                (out, 1)
            }
            None => (line.to_string(), 0),
        }
    }
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
    /// Digits typed before a command: the `3` in `3dd`. `None` until the first
    /// digit, which is what lets `0` keep meaning "start of line".
    count: Option<usize>,
    /// Where an incremental search started from.
    search_origin: Cursor,
    /// The fixed end of a visual selection; the cursor is the moving end.
    visual_anchor: Cursor,
    /// Register backing `yy`, `dd`, `p`, `P` and the visual operators.
    register: Option<Register>,
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
            count: None,
            search_origin: Cursor::default(),
            visual_anchor: Cursor::default(),
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
            Mode::Visual | Mode::VisualLine => self.handle_visual(key),
        }
    }

    // ── Visual mode ─────────────────────────────────────────────

    /// The selection in document order, as inclusive `(row, col)` bounds.
    ///
    /// `None` outside the visual modes, so a caller cannot accidentally act on
    /// a stale anchor.
    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        if !self.mode.is_visual() {
            return None;
        }

        let anchor = (self.visual_anchor.row, self.visual_anchor.col);
        let head = (self.cursor.row, self.cursor.col);
        let (start, end) = if anchor <= head { (anchor, head) } else { (head, anchor) };

        match self.mode {
            // Line-wise ignores the columns entirely: the span is whole lines
            // from the first column to the last character of the last line.
            Mode::VisualLine => {
                let last_col = self.document.buffer.line_len(end.0).saturating_sub(1);
                Some(((start.0, 0), (end.0, last_col)))
            }
            _ => Some((start, end)),
        }
    }

    fn enter_visual(&mut self, line_wise: bool) {
        self.mode = if line_wise { Mode::VisualLine } else { Mode::Visual };
        self.visual_anchor = self.cursor;
        self.count = None;
        self.pending = None;
        self.message.clear();
    }

    fn leave_visual(&mut self) {
        self.mode = Mode::Normal;
        self.count = None;
        self.pending = None;
        self.cursor.clamp(&self.document.buffer, self.mode);
    }

    /// Copies the selection into the register, tagged with how it was made.
    fn yank_selection(&mut self) {
        let Some((start, end)) = self.selection() else { return };

        if self.mode == Mode::VisualLine {
            let lines: Vec<String> = (start.0..=end.0)
                .filter_map(|row| self.document.buffer.line(row).map(str::to_string))
                .collect();
            let count = lines.len();
            self.register = Some(Register::Lines(lines));
            self.notify(count_message(count, "yanked"), Level::Info);
        } else {
            let text = self.document.buffer.range_text(start, end);
            let chars = text.chars().count();
            self.register = Some(Register::Chars(text));
            let noun = if chars == 1 { "character" } else { "characters" };
            self.notify(format!("{chars} {noun} yanked"), Level::Info);
        }

        // Vim leaves the cursor at the start of what was yanked.
        self.cursor = Cursor { row: start.0, col: start.1 };
        self.leave_visual();
    }

    /// Deletes the selection, yanking it first so `p` can put it back.
    fn delete_selection(&mut self, then_insert: bool) {
        let Some((start, end)) = self.selection() else { return };
        let line_wise = self.mode == Mode::VisualLine;

        self.checkpoint();

        if line_wise {
            let lines: Vec<String> = (start.0..=end.0)
                .filter_map(|row| self.document.buffer.line(row).map(str::to_string))
                .collect();
            let removed = lines.len();
            self.register = Some(Register::Lines(lines));
            self.cursor = Cursor { row: start.0, col: 0 };
            for _ in 0..removed {
                self.document.buffer.delete_line(start.0);
            }
            // `c` on whole lines leaves an empty one to type into, rather than
            // dropping the user onto the following line.
            if then_insert {
                self.document.buffer.insert_line(start.0, String::new());
            }
            self.notify(count_message(removed, "deleted and yanked"), Level::Info);
        } else {
            let text = self.document.buffer.range_text(start, end);
            let chars = text.chars().count();
            self.register = Some(Register::Chars(text));
            self.document.buffer.delete_range(start, end);
            self.cursor = Cursor { row: start.0, col: start.1 };
            let noun = if chars == 1 { "character" } else { "characters" };
            self.notify(format!("{chars} {noun} deleted and yanked"), Level::Info);
        }

        if then_insert {
            self.mode = Mode::Insert;
            self.count = None;
            self.pending = None;
        } else {
            self.leave_visual();
        }
        self.after_edit();
    }

    fn handle_visual(&mut self, key: KeyEvent) {
        // Counts work here too: `3j` extends the selection three lines.
        if let KeyCode::Char(digit @ '0'..='9') = key.code {
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && (digit != '0' || self.count.is_some())
            {
                let value = digit as usize - '0' as usize;
                self.count = Some(
                    self.count
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(value)
                        .min(1_000_000),
                );
                return;
            }
        }

        if let Some('g') = self.pending.take() {
            if key.code == KeyCode::Char('g') {
                self.count = None;
                self.cursor.buffer_start();
                return;
            }
        }

        let key_had_count = self.count.is_some();
        let count = self.take_count();

        match key.code {
            KeyCode::Esc => self.leave_visual(),

            // Motions move the head; the anchor stays put.
            KeyCode::Char('h') | KeyCode::Left => self.repeat(count, |app| app.cursor.left()),
            KeyCode::Char('l') | KeyCode::Right => {
                self.repeat(count, |app| app.cursor.right(&app.document.buffer, app.mode))
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.repeat(count, |app| app.cursor.up(&app.document.buffer, app.mode))
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.repeat(count, |app| app.cursor.down(&app.document.buffer, app.mode))
            }
            KeyCode::Char('0') | KeyCode::Home => self.cursor.line_start(),
            KeyCode::Char('$') | KeyCode::End => self.cursor.line_end(&self.document.buffer, self.mode),
            KeyCode::Char('G') => match key_had_count {
                true => self.goto_line(count),
                false => self.cursor.buffer_end(&self.document.buffer, self.mode),
            },
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('w') => self.repeat(count, |app| app.word_forward()),
            KeyCode::Char('b') => self.repeat(count, |app| app.word_back()),

            // Switching between the two visual flavours without losing the
            // anchor, and toggling back out by pressing the same key again.
            KeyCode::Char('v') => {
                if self.mode == Mode::Visual {
                    self.leave_visual();
                } else {
                    self.mode = Mode::Visual;
                }
            }
            KeyCode::Char('V') => {
                if self.mode == Mode::VisualLine {
                    self.leave_visual();
                } else {
                    self.mode = Mode::VisualLine;
                }
            }
            // `o` jumps to the other end, so a selection started in the wrong
            // direction can be fixed without starting over.
            KeyCode::Char('o') => std::mem::swap(&mut self.cursor, &mut self.visual_anchor),

            // Operators
            KeyCode::Char('y') => self.yank_selection(),
            KeyCode::Char('d') | KeyCode::Char('x') => self.delete_selection(false),
            KeyCode::Char('c') | KeyCode::Char('s') => self.delete_selection(true),
            KeyCode::Char('p') => self.replace_selection_with_register(),

            _ => {}
        }
    }

    /// `p` over a selection: replace what is highlighted with the register.
    fn replace_selection_with_register(&mut self) {
        if self.register.is_none() {
            self.notify("register empty — use yy or dd first", Level::Warn);
            return;
        }
        let register = self.register.clone();
        self.delete_selection(false);
        // delete_selection overwrote the register with what it removed, which
        // is right for `d` and wrong here: the user asked to paste what they
        // had, not what they just deleted.
        let removed = std::mem::replace(&mut self.register, register);
        match self.register.clone() {
            Some(Register::Chars(text)) => {
                self.checkpoint();
                let landed = self
                    .document
                    .buffer
                    .insert_text(self.cursor.row, self.cursor.col, &text);
                self.cursor = Cursor { row: landed.0, col: landed.1 };
                self.after_edit();
            }
            Some(Register::Lines(lines)) => {
                self.checkpoint();
                for (index, line) in lines.iter().enumerate() {
                    self.document.buffer.insert_line(self.cursor.row + index, line.clone());
                }
                self.cursor.col = 0;
                self.after_edit();
            }
            None => {}
        }
        // What was replaced becomes the register, exactly as Vim does it.
        self.register = removed;
        self.notify("selection replaced", Level::Info);
    }

    /// The count typed before the current command, or 1.
    ///
    /// Reading it clears it: a count belongs to exactly one command, and
    /// leaking it into the next keystroke is how `2j` quietly becomes `2x`.
    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1).max(1)
    }

    /// The digits typed so far, for the status bar to echo back.
    pub fn pending_count(&self) -> Option<usize> {
        self.count
    }

    fn handle_normal(&mut self, key: KeyEvent) {
        // Digits build a count — except a leading `0`, which is the motion to
        // the start of the line. Once a count is under way, `0` is just a
        // digit, so `10j` means ten lines down and not "line start, then j".
        if let KeyCode::Char(digit @ '0'..='9') = key.code {
            if !key.modifiers.contains(KeyModifiers::CONTROL)
                && (digit != '0' || self.count.is_some())
            {
                let value = digit as usize - '0' as usize;
                // Saturating: a user leaning on a digit key must not overflow
                // into a panic or a wrapped-around tiny count.
                self.count = Some(
                    self.count
                        .unwrap_or(0)
                        .saturating_mul(10)
                        .saturating_add(value)
                        .min(1_000_000),
                );
                return;
            }
        }

        // Two-stroke commands: if one is pending, this key completes it.
        if let Some(first) = self.pending.take() {
            match (first, key.code) {
                ('g', KeyCode::Char('g')) => {
                    self.count = None;
                    self.cursor.buffer_start();
                    return;
                }
                ('d', KeyCode::Char('d')) => {
                    let count = self.take_count();
                    self.delete_lines(count);
                    return;
                }
                ('y', KeyCode::Char('y')) => {
                    let count = self.take_count();
                    self.yank_lines(count);
                    return;
                }
                _ => {
                    // Sequence aborted: this key is processed as a normal one,
                    // and the count it may have carried goes with it.
                }
            }
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            self.redo();
            return;
        }

        // Captured before the count is consumed: `G` and `12G` are different
        // commands, and only the presence of digits tells them apart.
        let key_had_count = self.count.is_some();
        let count = self.take_count();

        match key.code {
            // Motion
            KeyCode::Char('h') | KeyCode::Left => self.repeat(count, |app| app.cursor.left()),
            KeyCode::Char('l') | KeyCode::Right => {
                self.repeat(count, |app| app.cursor.right(&app.document.buffer, app.mode))
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.repeat(count, |app| app.cursor.up(&app.document.buffer, app.mode))
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.repeat(count, |app| app.cursor.down(&app.document.buffer, app.mode))
            }
            KeyCode::Char('0') | KeyCode::Home => self.cursor.line_start(),
            KeyCode::Char('$') | KeyCode::End => self.cursor.line_end(&self.document.buffer, self.mode),
            // Bare `G` goes to the last line; `12G` goes to line 12, counting
            // from one the way the line numbers on screen do.
            KeyCode::Char('G') => match key_had_count {
                true => self.goto_line(count),
                false => self.cursor.buffer_end(&self.document.buffer, self.mode),
            },
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('w') => self.repeat(count, |app| app.word_forward()),
            KeyCode::Char('b') => self.repeat(count, |app| app.word_back()),

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
            KeyCode::Char('x') => self.delete_chars(count),
            KeyCode::Char('d') => {
                // The count belongs to the whole `3dd`, so it has to survive
                // the wait for the second key.
                self.count = key_had_count.then_some(count);
                self.pending = Some('d');
            }
            KeyCode::Char('y') => {
                self.count = key_had_count.then_some(count);
                self.pending = Some('y');
            }
            KeyCode::Char('p') => self.paste_line(false, count),
            KeyCode::Char('P') => self.paste_line(true, count),

            // Selection
            KeyCode::Char('v') => self.enter_visual(false),
            KeyCode::Char('V') => self.enter_visual(true),
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

    /// Text arriving from the terminal's bracketed paste.
    ///
    /// Without bracketed paste the terminal hands pasted text to the editor one
    /// keystroke at a time, so pasting into Normal mode runs every character as
    /// a command — which is how a pasted config block turns into a scattering
    /// of deletions and mode changes. With it, the whole block arrives as one
    /// event and is inserted as text, never interpreted.
    ///
    /// Character-wise in Insert mode, where the user is already typing into a
    /// line; line-wise in Normal mode, like `p`, because that is what pasting a
    /// block of configuration into a file actually means.
    pub fn paste_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        // A terminal may deliver either terminator; neither should end up in
        // the buffer as a character.
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        let lines: Vec<&str> = normalized.split('\n').collect();

        self.checkpoint();

        match self.mode {
            Mode::Insert => {
                for (index, line) in lines.iter().enumerate() {
                    if index > 0 {
                        self.document.buffer.insert_newline(self.cursor.row, self.cursor.col);
                        self.cursor.row += 1;
                        self.cursor.col = 0;
                    }
                    for ch in line.chars() {
                        self.document.buffer.insert_char(self.cursor.row, self.cursor.col, ch);
                        self.cursor.col += 1;
                    }
                }
            }
            _ => {
                let start = self.cursor.row + 1;
                for (index, line) in lines.iter().enumerate() {
                    self.document.buffer.insert_line(start + index, (*line).to_string());
                }
                self.cursor.row = start;
                self.cursor.col = 0;
            }
        }

        self.after_edit();
        self.notify(count_message(lines.len(), "pasted"), Level::Info);
    }

    /// Runs a motion `count` times.
    ///
    /// Each step re-reads the buffer, so the motion clamps at the edges the
    /// same way it does when pressed by hand: `99j` on a five-line file lands
    /// on the last line, it does not run off the end.
    fn repeat(&mut self, count: usize, mut step: impl FnMut(&mut Self)) {
        for _ in 0..count {
            step(self);
        }
    }

    /// `NG`: jump to a line, counted from one.
    fn goto_line(&mut self, line: usize) {
        let last = self.document.buffer.line_count().saturating_sub(1);
        self.cursor.row = line.saturating_sub(1).min(last);
        self.cursor.col = 0;
        self.cursor.clamp(&self.document.buffer, self.mode);
    }

    /// `Nx`: delete up to `count` characters from the cursor, stopping at the
    /// end of the line rather than eating the newline.
    fn delete_chars(&mut self, count: usize) {
        let available = self
            .document
            .buffer
            .line_len(self.cursor.row)
            .saturating_sub(self.cursor.col);
        let to_delete = count.min(available);
        if to_delete == 0 {
            return;
        }

        self.checkpoint();
        for _ in 0..to_delete {
            self.document.buffer.delete_char(self.cursor.row, self.cursor.col);
        }
        self.after_edit();
    }

    /// The lines a counted line-wise operator would cover, clamped to the end
    /// of the buffer: `9dd` near the bottom deletes what is left, like Vim.
    fn lines_from_cursor(&self, count: usize) -> Vec<String> {
        let last = self.document.buffer.line_count();
        (self.cursor.row..(self.cursor.row + count).min(last))
            .filter_map(|row| self.document.buffer.line(row).map(str::to_string))
            .collect()
    }

    /// `Ndd`.
    fn delete_lines(&mut self, count: usize) {
        let lines = self.lines_from_cursor(count);
        if lines.is_empty() {
            return;
        }

        self.checkpoint();
        let removed = lines.len();
        self.register = Some(Register::Lines(lines));
        for _ in 0..removed {
            self.document.buffer.delete_line(self.cursor.row);
        }
        self.after_edit();
        self.notify(count_message(removed, "deleted and yanked"), Level::Info);
    }

    /// `Nyy`.
    fn yank_lines(&mut self, count: usize) {
        let lines = self.lines_from_cursor(count);
        if lines.is_empty() {
            return;
        }

        let yanked = lines.len();
        self.register = Some(Register::Lines(lines));
        self.notify(count_message(yanked, "yanked"), Level::Info);
    }

    /// `p` / `P` in Normal mode. Line registers open new lines; character
    /// registers land beside the cursor, which is where the text came from.
    fn paste_line(&mut self, above: bool, count: usize) {
        let Some(register) = self.register.clone() else {
            self.notify("register empty — use yy or dd first", Level::Warn);
            return;
        };

        self.checkpoint();

        match register {
            Register::Lines(lines) => {
                let start = if above { self.cursor.row } else { self.cursor.row + 1 };
                let mut row = start;
                for _ in 0..count {
                    for line in &lines {
                        self.document.buffer.insert_line(row, line.clone());
                        row += 1;
                    }
                }
                self.cursor.row = start;
                self.cursor.col = 0;
                self.after_edit();
                self.notify(count_message(lines.len() * count, "pasted"), Level::Info);
            }
            Register::Chars(text) => {
                // `p` puts after the cursor, `P` before it — the difference
                // that makes pasting a word inside a line land where expected.
                let col = if above { self.cursor.col } else { self.cursor.col + 1 };
                let mut landed = (self.cursor.row, col);
                for _ in 0..count {
                    landed = self.document.buffer.insert_text(landed.0, landed.1, &text);
                    landed.1 += 1;
                }
                self.cursor = Cursor { row: landed.0, col: landed.1.saturating_sub(1) };
                self.after_edit();
                let chars = text.chars().count() * count;
                let noun = if chars == 1 { "character" } else { "characters" };
                self.notify(format!("{chars} {noun} pasted"), Level::Info);
            }
        }
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
                } else if let Some(rule) = parse_substitution(other) {
                    self.substitute(&rule);
                } else {
                    self.notify(format!("unknown command: {other}"), Level::Error);
                }
            }
        }
    }

    /// Runs a parsed `:s`, reporting exactly what it touched.
    ///
    /// Nothing is checkpointed until a replacement is certain: a pattern that
    /// matches nothing must not push an undo step, or `u` would silently
    /// swallow the user's previous edit instead of the substitution they
    /// thought they had just made.
    fn substitute(&mut self, rule: &Substitution) {
        let rows: Vec<usize> = if rule.whole_file {
            (0..self.document.buffer.line_count()).collect()
        } else {
            vec![self.cursor.row]
        };

        let mut edits: Vec<(usize, String)> = Vec::new();
        let mut replacements = 0usize;

        for row in rows {
            let Some(line) = self.document.buffer.line(row) else { continue };
            let (new_line, count) = substitute_line(line, rule);
            if count > 0 {
                replacements += count;
                edits.push((row, new_line));
            }
        }

        if edits.is_empty() {
            self.notify(format!("pattern not found: {}", rule.pattern), Level::Warn);
            return;
        }

        self.checkpoint();
        let lines_touched = edits.len();
        // Land on the first changed line so the user sees the result without
        // hunting for it.
        let first_row = edits[0].0;
        for (row, text) in edits {
            self.document.buffer.replace_line(row, text);
        }

        self.cursor.row = first_row;
        self.cursor.col = 0;
        self.after_edit();

        let subject = if replacements == 1 { "replacement" } else { "replacements" };
        let scope = if lines_touched == 1 { "line" } else { "lines" };
        self.notify(
            format!("{replacements} {subject} on {lines_touched} {scope}"),
            Level::Info,
        );
    }

    fn show_info(&mut self) {
        let (lines, words, chars) = self.stats();
        let path = self
            .document
            .path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "[sin ruta]".to_string());
        self.notify(
            format!(
                "{path} · {lines} lines · {words} words · {chars} chars · {}",
                self.document.line_ending().label()
            ),
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

    /// Types a `:` command and runs it, the way a user would.
    fn command(app: &mut App, text: &str) {
        press(app, KeyCode::Char(':'));
        for ch in text.chars() {
            press(app, KeyCode::Char(ch));
        }
        press(app, KeyCode::Enter);
    }

    /// Types a sequence of plain characters, e.g. "3dd".
    fn keys(app: &mut App, sequence: &str) {
        for ch in sequence.chars() {
            press(app, KeyCode::Char(ch));
        }
    }

    // ── Counts ──────────────────────────────────────────────────

    #[test]
    fn a_count_multiplies_a_motion() {
        let mut app = app_with("a\nb\nc\nd\ne");
        keys(&mut app, "3j");
        assert_eq!(app.cursor.row, 3);
    }

    #[test]
    fn a_count_of_more_than_one_digit_works() {
        let mut app = app_with(&"x\n".repeat(30));
        keys(&mut app, "12j");
        assert_eq!(app.cursor.row, 12);
    }

    #[test]
    fn zero_is_a_motion_on_its_own_and_a_digit_inside_a_count() {
        let mut app = app_with("abcdef\nghijkl\nmnopqr");
        keys(&mut app, "lll");
        assert_eq!(app.cursor.col, 3);

        // Bare `0`: back to the start of the line, not a count.
        keys(&mut app, "0");
        assert_eq!(app.cursor.col, 0);
        assert_eq!(app.pending_count(), None);

        // `10` then a motion: ten of them, clamped by the buffer.
        keys(&mut app, "10j");
        assert_eq!(app.cursor.row, 2, "clamped at the last line");
    }

    #[test]
    fn a_motion_clamps_instead_of_running_off_the_end() {
        let mut app = app_with("uno\ndos");
        keys(&mut app, "99j");
        assert_eq!(app.cursor.row, 1);

        keys(&mut app, "99l");
        assert_eq!(app.cursor.col, 2, "last character of 'dos'");
    }

    #[test]
    fn a_count_is_spent_by_one_command_and_does_not_leak() {
        let mut app = app_with("abcdef\nghijkl");
        keys(&mut app, "3l");
        assert_eq!(app.cursor.col, 3);
        assert_eq!(app.pending_count(), None, "the count was consumed");

        // The next motion is a single step, not another three.
        keys(&mut app, "l");
        assert_eq!(app.cursor.col, 4);
    }

    #[test]
    fn a_count_deletes_that_many_characters() {
        let mut app = app_with("abcdef");
        keys(&mut app, "3x");
        assert_eq!(app.document.buffer.line(0), Some("def"));
    }

    #[test]
    fn deleting_more_characters_than_the_line_has_stops_at_the_end() {
        let mut app = app_with("ab\ncd");
        keys(&mut app, "9x");
        assert_eq!(app.document.buffer.line(0), Some(""));
        assert_eq!(app.document.buffer.line_count(), 2, "the newline survives");
    }

    #[test]
    fn a_count_deletes_that_many_lines() {
        let mut app = app_with("a\nb\nc\nd");
        keys(&mut app, "3dd");
        assert_eq!(app.document.buffer.to_text(), "d");
        assert_eq!(app.message, "3 lines deleted and yanked");
    }

    #[test]
    fn a_counted_delete_is_a_single_undo_step() {
        let mut app = app_with("a\nb\nc\nd");
        keys(&mut app, "3dd");
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.document.buffer.to_text(), "a\nb\nc\nd");
    }

    #[test]
    fn a_counted_yank_and_paste_round_trip_every_line() {
        let mut app = app_with("uno\ndos\ntres");
        keys(&mut app, "2yy");
        assert_eq!(app.message, "2 lines yanked");

        keys(&mut app, "G");
        press(&mut app, KeyCode::Char('p'));
        assert_eq!(app.document.buffer.to_text(), "uno\ndos\ntres\nuno\ndos");
    }

    #[test]
    fn paste_can_be_counted_too() {
        let mut app = app_with("uno\ndos");
        keys(&mut app, "yy");
        keys(&mut app, "3p");
        assert_eq!(app.document.buffer.to_text(), "uno\nuno\nuno\nuno\ndos");
    }

    #[test]
    fn deleting_past_the_end_of_the_buffer_takes_what_is_there() {
        let mut app = app_with("a\nb");
        keys(&mut app, "9dd");
        assert_eq!(app.document.buffer.line_count(), 1);
        assert_eq!(app.document.buffer.line(0), Some(""));
    }

    #[test]
    fn capital_g_with_a_count_jumps_to_that_line() {
        let mut app = app_with("1\n2\n3\n4\n5");
        keys(&mut app, "3G");
        assert_eq!(app.cursor.row, 2, "lines are counted from one");

        // Bare G still means the last line.
        keys(&mut app, "G");
        assert_eq!(app.cursor.row, 4);

        // Past the end, clamp rather than jump into nothing.
        keys(&mut app, "99G");
        assert_eq!(app.cursor.row, 4);
    }

    #[test]
    fn an_aborted_two_stroke_sequence_drops_its_count() {
        let mut app = app_with("abcdef\nghijkl");
        // `3d` then a key that does not complete an operator: nothing happens
        // to the text, and the 3 must not linger for the next command.
        keys(&mut app, "3d");
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.document.buffer.to_text(), "abcdef\nghijkl");

        keys(&mut app, "l");
        assert_eq!(app.cursor.col, 1, "one step, not three");
    }

    // ── Visual mode ─────────────────────────────────────────────

    #[test]
    fn v_starts_a_selection_and_esc_ends_it() {
        let mut app = app_with("hello");
        keys(&mut app, "v");
        assert_eq!(app.mode, Mode::Visual);
        assert_eq!(app.selection(), Some(((0, 0), (0, 0))), "one character to start");

        keys(&mut app, "ll");
        assert_eq!(app.selection(), Some(((0, 0), (0, 2))));

        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.selection(), None);
    }

    #[test]
    fn a_selection_made_backwards_still_reads_forwards() {
        let mut app = app_with("hello");
        keys(&mut app, "lll"); // on the second 'l'
        keys(&mut app, "vhh"); // select leftwards
        assert_eq!(app.selection(), Some(((0, 1), (0, 3))));
    }

    #[test]
    fn visual_delete_removes_exactly_what_was_highlighted() {
        let mut app = app_with("hello world");
        keys(&mut app, "vllll"); // "hello"
        keys(&mut app, "d");

        assert_eq!(app.document.buffer.line(0), Some(" world"));
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.message, "5 characters deleted and yanked");
    }

    #[test]
    fn visual_yank_then_paste_puts_the_span_beside_the_cursor() {
        let mut app = app_with("abc");
        keys(&mut app, "vly"); // yank "ab"
        assert_eq!(app.cursor.col, 0, "the cursor returns to the start of the yank");

        keys(&mut app, "$"); // on 'c'
        keys(&mut app, "p");
        assert_eq!(app.document.buffer.line(0), Some("abcab"));
    }

    #[test]
    fn a_character_yank_pastes_inside_the_line_not_as_a_new_one() {
        let mut app = app_with("xy\nsecond");
        keys(&mut app, "vy"); // one character: "x"
        keys(&mut app, "p");

        assert_eq!(app.document.buffer.line_count(), 2, "no new line was opened");
        assert_eq!(app.document.buffer.line(0), Some("xxy"));
    }

    #[test]
    fn visual_selection_spans_lines() {
        let mut app = app_with("uno\ndos\ntres");
        keys(&mut app, "lvj"); // from (0,1) to (1,1)
        assert_eq!(app.selection(), Some(((0, 1), (1, 1))));

        keys(&mut app, "d");
        assert_eq!(app.document.buffer.to_text(), "us\ntres");
    }

    #[test]
    fn capital_v_selects_whole_lines_whatever_the_columns() {
        let mut app = app_with("uno\ndos largo\ntres");
        keys(&mut app, "llV"); // column 2, line-wise
        assert_eq!(app.selection(), Some(((0, 0), (0, 2))), "the whole first line");

        keys(&mut app, "j");
        assert_eq!(
            app.selection(),
            Some(((0, 0), (1, 8))),
            "both lines, to the end of the longer one"
        );
    }

    #[test]
    fn line_wise_delete_takes_whole_lines_and_yanks_them_as_lines() {
        let mut app = app_with("uno\ndos\ntres");
        keys(&mut app, "Vj");
        keys(&mut app, "d");
        assert_eq!(app.document.buffer.to_text(), "tres");
        assert_eq!(app.message, "2 lines deleted and yanked");

        // A line register opens new lines when pasted, unlike a character one.
        keys(&mut app, "p");
        assert_eq!(app.document.buffer.to_text(), "tres\nuno\ndos");
    }

    #[test]
    fn c_over_a_selection_deletes_it_and_drops_into_insert() {
        let mut app = app_with("hello world");
        keys(&mut app, "vllll"); // "hello"
        keys(&mut app, "c");

        assert_eq!(app.mode, Mode::Insert);
        assert_eq!(app.document.buffer.line(0), Some(" world"));

        keys(&mut app, "adios");
        assert_eq!(app.document.buffer.line(0), Some("adios world"));
    }

    #[test]
    fn line_wise_change_leaves_an_empty_line_to_type_into() {
        let mut app = app_with("uno\ndos");
        keys(&mut app, "Vc");
        assert_eq!(app.mode, Mode::Insert);
        assert_eq!(app.document.buffer.to_text(), "\ndos");
    }

    #[test]
    fn o_swaps_the_ends_of_a_selection() {
        let mut app = app_with("abcdef");
        keys(&mut app, "vll"); // anchor 0, head 2
        keys(&mut app, "o");
        assert_eq!(app.cursor.col, 0);
        assert_eq!(app.visual_anchor.col, 2);

        // Extending now grows the *other* end.
        keys(&mut app, "h");
        assert_eq!(app.selection(), Some(((0, 0), (0, 2))));
    }

    #[test]
    fn v_and_capital_v_toggle_and_switch_without_losing_the_anchor() {
        let mut app = app_with("abcdef\nghijkl");
        keys(&mut app, "vll");
        keys(&mut app, "V");
        assert_eq!(app.mode, Mode::VisualLine);
        assert_eq!(app.selection(), Some(((0, 0), (0, 5))), "same line, now whole");

        keys(&mut app, "v");
        assert_eq!(app.mode, Mode::Visual);
        assert_eq!(app.selection(), Some(((0, 0), (0, 2))), "the anchor survived");

        keys(&mut app, "v");
        assert_eq!(app.mode, Mode::Normal, "pressing v again leaves visual mode");
    }

    #[test]
    fn a_count_extends_the_selection() {
        let mut app = app_with("a\nb\nc\nd\ne");
        keys(&mut app, "v3j");
        assert_eq!(app.selection(), Some(((0, 0), (3, 0))));
    }

    #[test]
    fn a_visual_delete_is_one_undo_step() {
        let mut app = app_with("hello world");
        keys(&mut app, "vllll");
        keys(&mut app, "d");
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.document.buffer.line(0), Some("hello world"));
    }

    #[test]
    fn p_over_a_selection_swaps_it_for_the_register() {
        let mut app = app_with("uno\ndos");
        keys(&mut app, "yy"); // register: the line "uno"
        keys(&mut app, "jVp"); // replace the second line with it
        assert_eq!(app.document.buffer.to_text(), "uno\nuno");
    }

    #[test]
    fn visual_operators_respect_multibyte_text() {
        let mut app = app_with("café con leche");
        keys(&mut app, "vlll"); // "café"
        keys(&mut app, "d");
        assert_eq!(app.document.buffer.line(0), Some(" con leche"));
    }

    // ── Bracketed paste ─────────────────────────────────────────

    #[test]
    fn pasting_in_normal_mode_inserts_lines_instead_of_running_commands() {
        let mut app = app_with("primera\nsegunda");
        // Every character here is a Normal-mode command: without bracketed
        // paste this text would delete lines and enter insert mode.
        app.paste_text("dd\nxxx\n:q!");

        assert_eq!(
            app.document.buffer.to_text(),
            "primera\ndd\nxxx\n:q!\nsegunda"
        );
        assert!(!app.quit, "a pasted :q! must not quit the editor");
        assert_eq!(app.cursor.row, 1);
    }

    #[test]
    fn pasting_in_insert_mode_lands_at_the_cursor() {
        let mut app = app_with("ac");
        press(&mut app, KeyCode::Char('l')); // on 'c'
        press(&mut app, KeyCode::Char('i')); // insert before it
        app.paste_text("b");

        assert_eq!(app.document.buffer.line(0), Some("abc"));
    }

    #[test]
    fn a_multiline_paste_in_insert_mode_splits_the_line() {
        let mut app = app_with("ad");
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('i'));
        app.paste_text("b\nc");

        assert_eq!(app.document.buffer.to_text(), "ab\ncd");
    }

    #[test]
    fn a_paste_is_one_undo_step() {
        let mut app = app_with("uno");
        app.paste_text("dos\ntres");
        assert_eq!(app.document.buffer.line_count(), 3);

        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.document.buffer.to_text(), "uno");
    }

    #[test]
    fn pasted_carriage_returns_never_reach_the_buffer() {
        // Text copied from a Windows editor arrives with CRLF.
        let mut app = app_with("uno");
        app.paste_text("dos\r\ntres");

        assert_eq!(app.document.buffer.to_text(), "uno\ndos\ntres");
        assert!(app.document.buffer.iter_lines().all(|line| !line.contains('\r')));
    }

    #[test]
    fn an_empty_paste_does_nothing_at_all() {
        let mut app = app_with("uno");
        app.paste_text("");
        assert_eq!(app.document.buffer.to_text(), "uno");
        assert_eq!(app.message, "");
    }

    // ── :s substitution ─────────────────────────────────────────

    #[test]
    fn parses_the_basic_substitution_forms() {
        assert_eq!(
            parse_substitution("s/foo/bar/"),
            Some(Substitution {
                pattern: "foo".into(),
                replacement: "bar".into(),
                global: false,
                whole_file: false,
            })
        );
        assert_eq!(
            parse_substitution("%s/foo/bar/g"),
            Some(Substitution {
                pattern: "foo".into(),
                replacement: "bar".into(),
                global: true,
                whole_file: true,
            })
        );
        // A trailing delimiter is optional, and so is the replacement: `:s/x/`
        // deletes every x it touches.
        assert_eq!(
            parse_substitution("s/x/"),
            Some(Substitution {
                pattern: "x".into(),
                replacement: String::new(),
                global: false,
                whole_file: false,
            })
        );
    }

    #[test]
    fn any_punctuation_can_be_the_delimiter() {
        // The reason this matters: rewriting a path without escaping slashes.
        let rule = parse_substitution("%s#/usr/bin#/bin#g").expect("parses");
        assert_eq!(rule.pattern, "/usr/bin");
        assert_eq!(rule.replacement, "/bin");
        assert!(rule.global);
    }

    #[test]
    fn the_delimiter_can_still_be_escaped_inside_a_field() {
        let rule = parse_substitution("s/a\\/b/c/").expect("parses");
        assert_eq!(rule.pattern, "a/b");
        assert_eq!(rule.replacement, "c");
    }

    #[test]
    fn a_backslash_before_anything_else_stays_literal() {
        // Config files are full of these; swallowing them would corrupt paths.
        let rule = parse_substitution("s/C:\\temp/D:\\temp/").expect("parses");
        assert_eq!(rule.pattern, "C:\\temp");
        assert_eq!(rule.replacement, "D:\\temp");
    }

    #[test]
    fn rejects_things_that_are_not_substitutions() {
        assert_eq!(parse_substitution("set number"), None);
        assert_eq!(parse_substitution("sort"), None);
        assert_eq!(parse_substitution("s"), None);
        assert_eq!(parse_substitution("s//bar/"), None, "an empty pattern is meaningless");
        assert_eq!(parse_substitution("s/a/b/x"), None, "unknown flag");
    }

    #[test]
    fn substitutes_the_first_match_on_the_current_line() {
        let mut app = app_with("uno dos uno\nuno");
        command(&mut app, "s/uno/UNO/");

        assert_eq!(app.document.buffer.line(0), Some("UNO dos uno"));
        assert_eq!(app.document.buffer.line(1), Some("uno"), "other lines untouched");
        assert_eq!(app.message, "1 replacement on 1 line");
    }

    #[test]
    fn the_g_flag_takes_every_match_on_the_line() {
        let mut app = app_with("uno dos uno");
        command(&mut app, "s/uno/UNO/g");

        assert_eq!(app.document.buffer.line(0), Some("UNO dos UNO"));
        assert_eq!(app.message, "2 replacements on 1 line");
    }

    #[test]
    fn percent_reaches_the_whole_file() {
        let mut app = app_with("uno\ndos uno\ntres");
        command(&mut app, "%s/uno/UNO/g");

        assert_eq!(app.document.buffer.to_text(), "UNO\ndos UNO\ntres");
        assert_eq!(app.message, "2 replacements on 2 lines");
    }

    #[test]
    fn a_substitution_is_one_undo_step() {
        let mut app = app_with("uno\nuno\nuno");
        command(&mut app, "%s/uno/dos/g");
        assert_eq!(app.document.buffer.to_text(), "dos\ndos\ndos");

        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.document.buffer.to_text(), "uno\nuno\nuno");
    }

    #[test]
    fn a_pattern_that_matches_nothing_changes_nothing_and_costs_no_undo_step() {
        let mut app = app_with("uno");
        // A real edit first, so there is something on the undo stack to lose.
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.document.buffer.line(0), Some("no"));

        command(&mut app, "s/zzz/algo/");
        assert_eq!(app.message, "pattern not found: zzz");
        assert_eq!(app.level, Level::Warn);

        // `u` must undo the `x`, not a no-op substitution.
        press(&mut app, KeyCode::Char('u'));
        assert_eq!(app.document.buffer.line(0), Some("uno"));
    }

    #[test]
    fn substitution_can_delete_by_replacing_with_nothing() {
        let mut app = app_with("quitar esto");
        command(&mut app, "s/ esto//");
        assert_eq!(app.document.buffer.line(0), Some("quitar"));
    }

    #[test]
    fn the_cursor_lands_on_the_first_line_it_changed() {
        let mut app = app_with("nada\nnada\naqui\nnada");
        command(&mut app, "%s/aqui/alli/");
        assert_eq!(app.cursor.row, 2);
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
