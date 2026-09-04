//! Syntax highlighting: a small lexer per language, resumable line by line.
//!
//! WHY NOT A GRAMMAR LIBRARY
//!   `syntect` carries Sublime grammars and an Oniguruma-class regex engine;
//!   `tree-sitter` compiles a parser per language from C. Either would be the
//!   right answer for an editor that wants to highlight anything. This one
//!   wants to highlight the handful of things you actually edit on a server —
//!   a unit file, an `sshd_config`, a shell script, a `Cargo.toml` — inside a
//!   binary small enough to ship in an appliance image, whose whole dependency
//!   tree is four crates. So the lexers are hand-written and deliberately
//!   shallow: they colour tokens, they do not parse programs.
//!
//! WHAT MAKES IT INCREMENTAL
//!   Highlighting one line needs to know what the previous line left open — a
//!   `/* ... */` that has not closed, a string that runs on. So each lexer is
//!   a function from `(state at the start of the line, the line)` to `(tokens,
//!   state at the end)`.
//!
//!   That makes the whole thing resumable: [`Highlighter`] caches the state
//!   entering every line, so drawing the visible window costs the lines in it
//!   plus, once, the lines above. An edit on line 500 invalidates 500 onwards
//!   and nothing before it — where re-lexing the file on every keystroke would
//!   make a 10 000-line file unusable.

use super::buffer::Buffer;
use std::path::Path;

/// What a token is, as far as colouring is concerned.
///
/// Deliberately four. A palette with thirty token classes needs thirty colours
/// that stay distinguishable on a 16-colour appliance console, and nobody can
/// name them apart at a glance anyway.
///
/// There is no `Text` variant: a lexer says "ordinary text" by emitting no
/// token at all, and the renderer paints whatever it was not told about in the
/// base style. A variant that no lexer ever produces would be a lie in the
/// type, and one more arm every caller has to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    /// A string or character literal, quotes included.
    Str,
    Comment,
    Number,
}

/// A run of characters sharing one kind. Ranges are **character** indices, to
/// match everything else in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub start: usize,
    pub end: usize,
    pub kind: TokenKind,
}

/// What a line leaves open for the next one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineState {
    #[default]
    Normal,
    /// Inside a `/* */`, with the nesting depth Rust allows.
    BlockComment(u8),
    /// Inside a fenced code block in Markdown.
    FencedCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Toml,
    /// INI-style config: `key = value`, `[section]`, `#` or `;` comments. What
    /// most things under `/etc` look like.
    Config,
    Shell,
    Json,
    Markdown,
    /// No highlighting. The honest answer for a file we do not know.
    PlainText,
}

impl Language {
    /// Detects from the file name, falling back to the first line.
    ///
    /// Extension first because it is nearly always right and costs nothing;
    /// the shebang only matters for the scripts that have no extension, which
    /// is most of the ones in `/usr/local/bin`.
    pub fn detect(path: Option<&Path>, first_line: &str) -> Self {
        if let Some(path) = path {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();

            if let Some(language) = Self::from_file_name(&name) {
                return language;
            }
        }

        if first_line.starts_with("#!") {
            let line = first_line.to_ascii_lowercase();
            if ["sh", "bash", "zsh", "dash", "ash"].iter().any(|s| line.contains(s)) {
                return Language::Shell;
            }
        }

        Language::PlainText
    }

    fn from_file_name(name: &str) -> Option<Self> {
        // Whole names first: these have no extension to go on.
        match name {
            "cargo.lock" => return Some(Language::Toml),
            "sshd_config" | "ssh_config" | "fstab" | "hosts" | "crontab" | "sudoers"
            | "nftables.conf" | "resolv.conf" => return Some(Language::Config),
            "makefile" | "dockerfile" => return Some(Language::Shell),
            _ => {}
        }

        let extension = name.rsplit_once('.').map(|(_, ext)| ext)?;
        Some(match extension {
            "rs" => Language::Rust,
            "toml" => Language::Toml,
            "conf" | "cfg" | "ini" | "service" | "socket" | "timer" | "target" | "mount" => {
                Language::Config
            }
            "sh" | "bash" | "zsh" | "env" => Language::Shell,
            "json" => Language::Json,
            "md" | "markdown" => Language::Markdown,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::Toml => "toml",
            Language::Config => "config",
            Language::Shell => "shell",
            Language::Json => "json",
            Language::Markdown => "markdown",
            Language::PlainText => "text",
        }
    }

    /// Lexes one line, given what the previous one left open.
    pub fn lex(self, line: &str, state: LineState) -> (Vec<Token>, LineState) {
        match self {
            Language::Rust => lex_rust(line, state),
            Language::Toml | Language::Config => lex_config(line, self),
            Language::Shell => lex_shell(line),
            Language::Json => lex_json(line),
            Language::Markdown => lex_markdown(line, state),
            Language::PlainText => (Vec::new(), LineState::Normal),
        }
    }
}

const RUST_KEYWORDS: [&str; 36] = [
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
    "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
    "mut", "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "type",
    "unsafe", "use", "where",
];

const SHELL_KEYWORDS: [&str; 22] = [
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "return", "in", "select", "time", "export", "local", "readonly", "set", "unset",
];

/// Rust: line and nesting block comments, strings with escapes, char literals,
/// numbers and keywords.
fn lex_rust(line: &str, state: LineState) -> (Vec<Token>, LineState) {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    // Resume an unterminated block comment from the previous line.
    let mut depth = match state {
        LineState::BlockComment(depth) => depth,
        _ => 0,
    };
    if depth > 0 {
        let start = 0;
        while index < chars.len() {
            if chars[index] == '*' && chars.get(index + 1) == Some(&'/') {
                depth -= 1;
                index += 2;
                if depth == 0 {
                    break;
                }
            } else if chars[index] == '/' && chars.get(index + 1) == Some(&'*') {
                depth += 1;
                index += 2;
            } else {
                index += 1;
            }
        }
        tokens.push(Token { start, end: index, kind: TokenKind::Comment });
        if depth > 0 {
            return (tokens, LineState::BlockComment(depth));
        }
    }

    while index < chars.len() {
        let ch = chars[index];

        if ch == '/' && chars.get(index + 1) == Some(&'/') {
            tokens.push(Token { start: index, end: chars.len(), kind: TokenKind::Comment });
            return (tokens, LineState::Normal);
        }

        if ch == '/' && chars.get(index + 1) == Some(&'*') {
            let start = index;
            depth = 1;
            index += 2;
            while index < chars.len() && depth > 0 {
                if chars[index] == '*' && chars.get(index + 1) == Some(&'/') {
                    depth -= 1;
                    index += 2;
                } else if chars[index] == '/' && chars.get(index + 1) == Some(&'*') {
                    depth += 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            tokens.push(Token { start, end: index, kind: TokenKind::Comment });
            if depth > 0 {
                return (tokens, LineState::BlockComment(depth));
            }
            continue;
        }

        if ch == '"' {
            let start = index;
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' {
                    index += 2;
                    continue;
                }
                if chars[index] == '"' {
                    index += 1;
                    break;
                }
                index += 1;
            }
            tokens.push(Token { start, end: index.min(chars.len()), kind: TokenKind::Str });
            continue;
        }

        if ch.is_ascii_digit() {
            let start = index;
            while index < chars.len()
                && (chars[index].is_ascii_alphanumeric() || chars[index] == '_' || chars[index] == '.')
            {
                index += 1;
            }
            tokens.push(Token { start, end: index, kind: TokenKind::Number });
            continue;
        }

        if ch.is_alphabetic() || ch == '_' {
            let start = index;
            while index < chars.len() && (chars[index].is_alphanumeric() || chars[index] == '_') {
                index += 1;
            }
            let word: String = chars[start..index].iter().collect();
            if RUST_KEYWORDS.contains(&word.as_str()) {
                tokens.push(Token { start, end: index, kind: TokenKind::Keyword });
            }
            continue;
        }

        index += 1;
    }

    (tokens, LineState::Normal)
}

/// TOML and INI-style config: `#` / `;` comments, `[sections]` as keywords,
/// the key before `=` as a keyword, quoted values as strings, bare numbers.
fn lex_config(line: &str, language: Language) -> (Vec<Token>, LineState) {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();

    let first = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(0);
    if let Some(&ch) = chars.get(first) {
        if ch == '#' || (ch == ';' && language == Language::Config) {
            tokens.push(Token { start: first, end: chars.len(), kind: TokenKind::Comment });
            return (tokens, LineState::Normal);
        }
        if ch == '[' {
            tokens.push(Token { start: first, end: chars.len(), kind: TokenKind::Keyword });
            return (tokens, LineState::Normal);
        }
    }

    let mut index = first;
    if let Some(equals) = chars.iter().position(|&c| c == '=') {
        tokens.push(Token { start: first, end: equals, kind: TokenKind::Keyword });
        index = equals + 1;
    }

    while index < chars.len() {
        let ch = chars[index];
        if ch == '#' {
            tokens.push(Token { start: index, end: chars.len(), kind: TokenKind::Comment });
            break;
        }
        if ch == '"' || ch == '\'' {
            let quote = ch;
            let start = index;
            index += 1;
            while index < chars.len() && chars[index] != quote {
                index += 1;
            }
            index = (index + 1).min(chars.len());
            tokens.push(Token { start, end: index, kind: TokenKind::Str });
            continue;
        }
        if ch.is_ascii_digit() {
            let start = index;
            while index < chars.len() && (chars[index].is_ascii_alphanumeric() || chars[index] == '.') {
                index += 1;
            }
            tokens.push(Token { start, end: index, kind: TokenKind::Number });
            continue;
        }
        index += 1;
    }

    (tokens, LineState::Normal)
}

/// Shell: `#` comments, both quote styles, `$variables`, keywords.
fn lex_shell(line: &str) -> (Vec<Token>, LineState) {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    // A shebang is a comment to the shell and worth colouring as one.
    if line.starts_with("#!") {
        return (
            vec![Token { start: 0, end: chars.len(), kind: TokenKind::Comment }],
            LineState::Normal,
        );
    }

    while index < chars.len() {
        let ch = chars[index];

        if ch == '#' {
            tokens.push(Token { start: index, end: chars.len(), kind: TokenKind::Comment });
            break;
        }

        if ch == '"' || ch == '\'' {
            let quote = ch;
            let start = index;
            index += 1;
            while index < chars.len() {
                if quote == '"' && chars[index] == '\\' {
                    index += 2;
                    continue;
                }
                if chars[index] == quote {
                    index += 1;
                    break;
                }
                index += 1;
            }
            tokens.push(Token { start, end: index.min(chars.len()), kind: TokenKind::Str });
            continue;
        }

        if ch == '$' {
            let start = index;
            index += 1;
            if chars.get(index) == Some(&'{') {
                while index < chars.len() && chars[index] != '}' {
                    index += 1;
                }
                index = (index + 1).min(chars.len());
            } else {
                while index < chars.len() && (chars[index].is_alphanumeric() || chars[index] == '_') {
                    index += 1;
                }
            }
            tokens.push(Token { start, end: index, kind: TokenKind::Number });
            continue;
        }

        if ch.is_alphabetic() || ch == '_' {
            let start = index;
            while index < chars.len() && (chars[index].is_alphanumeric() || chars[index] == '_') {
                index += 1;
            }
            let word: String = chars[start..index].iter().collect();
            if SHELL_KEYWORDS.contains(&word.as_str()) {
                tokens.push(Token { start, end: index, kind: TokenKind::Keyword });
            }
            continue;
        }

        index += 1;
    }

    (tokens, LineState::Normal)
}

/// JSON: strings, numbers, and the three bare literals.
fn lex_json(line: &str) -> (Vec<Token>, LineState) {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];

        if ch == '"' {
            let start = index;
            index += 1;
            while index < chars.len() {
                if chars[index] == '\\' {
                    index += 2;
                    continue;
                }
                if chars[index] == '"' {
                    index += 1;
                    break;
                }
                index += 1;
            }
            let end = index.min(chars.len());
            // A string followed by a colon is a key, which reads better as a
            // keyword than as one more string in a wall of them.
            let is_key = chars[end..].iter().find(|c| !c.is_whitespace()) == Some(&':');
            let kind = if is_key { TokenKind::Keyword } else { TokenKind::Str };
            tokens.push(Token { start, end, kind });
            continue;
        }

        if ch.is_ascii_digit() || (ch == '-' && chars.get(index + 1).is_some_and(char::is_ascii_digit)) {
            let start = index;
            index += 1;
            while index < chars.len()
                && (chars[index].is_ascii_digit()
                    || chars[index] == '.'
                    || chars[index] == 'e'
                    || chars[index] == 'E'
                    || chars[index] == '-'
                    || chars[index] == '+')
            {
                index += 1;
            }
            tokens.push(Token { start, end: index, kind: TokenKind::Number });
            continue;
        }

        if ch.is_alphabetic() {
            let start = index;
            while index < chars.len() && chars[index].is_alphabetic() {
                index += 1;
            }
            let word: String = chars[start..index].iter().collect();
            if matches!(word.as_str(), "true" | "false" | "null") {
                tokens.push(Token { start, end: index, kind: TokenKind::Keyword });
            }
            continue;
        }

        index += 1;
    }

    (tokens, LineState::Normal)
}

/// Markdown: headings, fenced code, inline code, and quotes.
fn lex_markdown(line: &str, state: LineState) -> (Vec<Token>, LineState) {
    let chars: Vec<char> = line.chars().collect();
    let trimmed = line.trim_start();

    if trimmed.starts_with("```") {
        let next = match state {
            LineState::FencedCode => LineState::Normal,
            _ => LineState::FencedCode,
        };
        return (
            vec![Token { start: 0, end: chars.len(), kind: TokenKind::Comment }],
            next,
        );
    }

    if state == LineState::FencedCode {
        return (
            vec![Token { start: 0, end: chars.len(), kind: TokenKind::Str }],
            LineState::FencedCode,
        );
    }

    if trimmed.starts_with('#') {
        return (
            vec![Token { start: 0, end: chars.len(), kind: TokenKind::Keyword }],
            LineState::Normal,
        );
    }

    if trimmed.starts_with('>') {
        return (
            vec![Token { start: 0, end: chars.len(), kind: TokenKind::Comment }],
            LineState::Normal,
        );
    }

    // Inline code spans.
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        if chars[index] == '`' {
            let start = index;
            index += 1;
            while index < chars.len() && chars[index] != '`' {
                index += 1;
            }
            index = (index + 1).min(chars.len());
            tokens.push(Token { start, end: index, kind: TokenKind::Str });
            continue;
        }
        index += 1;
    }

    (tokens, LineState::Normal)
}

/// Caches the lexer state entering each line, so drawing a window into a large
/// file does not mean re-lexing everything above it on every frame.
#[derive(Debug)]
pub struct Highlighter {
    language: Language,
    /// `states[i]` is what line `i` starts in. Always has `states[0]`.
    states: Vec<LineState>,
}

impl Highlighter {
    pub fn new(language: Language) -> Self {
        Self { language, states: vec![LineState::Normal] }
    }

    pub fn language(&self) -> Language {
        self.language
    }

    pub fn set_language(&mut self, language: Language) {
        self.language = language;
        self.states.truncate(1);
    }

    /// Everything from `row` on has to be lexed again. Called after an edit.
    ///
    /// Cheap on purpose: it is a truncation, so an edit near the top of a large
    /// file costs nothing until those lines are actually drawn.
    pub fn invalidate_from(&mut self, row: usize) {
        self.states.truncate(row + 1);
    }

    /// Tokens for one line, lexing only what the cache is missing.
    pub fn tokens(&mut self, buffer: &Buffer, row: usize) -> Vec<Token> {
        if self.language == Language::PlainText || row >= buffer.line_count() {
            return Vec::new();
        }
        self.ensure_state_for(buffer, row);
        let state = self.states.get(row).copied().unwrap_or_default();
        let line = buffer.line(row).unwrap_or("");
        self.language.lex(line, state).0
    }

    /// Fills the state cache up to and including `row`.
    fn ensure_state_for(&mut self, buffer: &Buffer, row: usize) {
        while self.states.len() <= row {
            let index = self.states.len() - 1;
            let state = self.states[index];
            let line = buffer.line(index).unwrap_or("");
            let (_, next) = self.language.lex(line, state);
            self.states.push(next);
        }
    }

    /// How many line states are cached — the measure of how much work was
    /// skipped, and what the incremental test asserts on.
    #[cfg(test)]
    pub fn cached_lines(&self) -> usize {
        self.states.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(language: Language, line: &str) -> Vec<(TokenKind, String)> {
        let chars: Vec<char> = line.chars().collect();
        language
            .lex(line, LineState::Normal)
            .0
            .into_iter()
            .map(|t| (t.kind, chars[t.start..t.end.min(chars.len())].iter().collect()))
            .collect()
    }

    // ── Detection ───────────────────────────────────────────────

    #[test]
    fn detects_by_extension() {
        let cases = [
            ("main.rs", Language::Rust),
            ("Cargo.toml", Language::Toml),
            ("deploy.sh", Language::Shell),
            ("package.json", Language::Json),
            ("README.md", Language::Markdown),
            ("gateway.conf", Language::Config),
            ("maat.service", Language::Config),
        ];
        for (name, expected) in cases {
            assert_eq!(Language::detect(Some(Path::new(name)), ""), expected, "{name}");
        }
    }

    #[test]
    fn detects_the_files_that_have_no_extension() {
        // The ones you actually edit on a server.
        for name in ["sshd_config", "fstab", "hosts", "sudoers"] {
            assert_eq!(Language::detect(Some(Path::new(name)), ""), Language::Config, "{name}");
        }
        assert_eq!(Language::detect(Some(Path::new("Makefile")), ""), Language::Shell);
    }

    #[test]
    fn falls_back_to_the_shebang() {
        assert_eq!(Language::detect(None, "#!/bin/bash"), Language::Shell);
        assert_eq!(Language::detect(Some(Path::new("backup")), "#!/bin/sh"), Language::Shell);
        assert_eq!(Language::detect(None, "just some prose"), Language::PlainText);
    }

    #[test]
    fn an_unknown_file_is_left_alone() {
        assert_eq!(Language::detect(Some(Path::new("notes.xyz")), "hello"), Language::PlainText);
        assert!(Language::PlainText.lex("fn main() {}", LineState::Normal).0.is_empty());
    }

    // ── Rust ────────────────────────────────────────────────────

    #[test]
    fn rust_colours_keywords_strings_numbers_and_comments() {
        let tokens = kinds(Language::Rust, r#"let x = 42; // note"#);
        assert_eq!(tokens[0], (TokenKind::Keyword, "let".into()));
        assert_eq!(tokens[1], (TokenKind::Number, "42".into()));
        assert_eq!(tokens[2], (TokenKind::Comment, "// note".into()));

        let tokens = kinds(Language::Rust, r#"println!("hola");"#);
        assert_eq!(tokens[0], (TokenKind::Str, "\"hola\"".into()));
    }

    #[test]
    fn a_string_containing_a_comment_marker_is_still_a_string() {
        let tokens = kinds(Language::Rust, r#"let url = "http://x"; // real"#);
        assert!(tokens.iter().any(|(k, t)| *k == TokenKind::Str && t.contains("http://x")));
        assert!(tokens.iter().any(|(k, t)| *k == TokenKind::Comment && t == "// real"));
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string() {
        let tokens = kinds(Language::Rust, r#"let s = "a\"b"; let y = 1;"#);
        assert_eq!(tokens[1], (TokenKind::Str, r#""a\"b""#.into()));
    }

    #[test]
    fn a_block_comment_carries_across_lines() {
        let (tokens, state) = Language::Rust.lex("let a = 1; /* opens", LineState::Normal);
        assert_eq!(state, LineState::BlockComment(1));
        assert_eq!(tokens.last().unwrap().kind, TokenKind::Comment);

        let (tokens, state) = Language::Rust.lex("still inside", state);
        assert_eq!(state, LineState::BlockComment(1));
        assert_eq!(tokens[0].kind, TokenKind::Comment);

        let (_, state) = Language::Rust.lex("closes */ let b = 2;", state);
        assert_eq!(state, LineState::Normal, "and the next line is code again");
    }

    #[test]
    fn nested_block_comments_need_both_closers() {
        let (_, state) = Language::Rust.lex("/* outer /* inner", LineState::Normal);
        assert_eq!(state, LineState::BlockComment(2));
        let (_, state) = Language::Rust.lex("*/ still in the outer", state);
        assert_eq!(state, LineState::BlockComment(1));
        let (_, state) = Language::Rust.lex("*/ done", state);
        assert_eq!(state, LineState::Normal);
    }

    // ── Config and TOML ─────────────────────────────────────────

    #[test]
    fn config_colours_sections_keys_and_values() {
        let tokens = kinds(Language::Toml, "[display]");
        assert_eq!(tokens[0], (TokenKind::Keyword, "[display]".into()));

        let tokens = kinds(Language::Toml, "tabwidth = 4  # columns");
        assert_eq!(tokens[0].0, TokenKind::Keyword);
        assert_eq!(tokens[1], (TokenKind::Number, "4".into()));
        assert_eq!(tokens[2], (TokenKind::Comment, "# columns".into()));
    }

    #[test]
    fn a_semicolon_comments_a_config_but_not_a_toml_file() {
        assert_eq!(kinds(Language::Config, "; a comment")[0].0, TokenKind::Comment);
        let toml = kinds(Language::Toml, "; not a comment in toml");
        assert!(toml.is_empty() || toml[0].0 != TokenKind::Comment);
    }

    // ── Shell ───────────────────────────────────────────────────

    #[test]
    fn shell_colours_keywords_variables_and_quotes() {
        let tokens = kinds(Language::Shell, r#"if [ -f "$HOME/.bashrc" ]; then"#);
        assert!(tokens.iter().any(|(k, t)| *k == TokenKind::Keyword && t == "if"));
        assert!(tokens.iter().any(|(k, t)| *k == TokenKind::Keyword && t == "then"));
        assert!(tokens.iter().any(|(k, _)| *k == TokenKind::Str));
    }

    #[test]
    fn a_shebang_is_a_comment() {
        assert_eq!(kinds(Language::Shell, "#!/bin/bash")[0].0, TokenKind::Comment);
    }

    #[test]
    fn a_braced_variable_ends_at_its_brace() {
        let tokens = kinds(Language::Shell, "echo ${NAME}x");
        assert!(tokens.iter().any(|(k, t)| *k == TokenKind::Number && t == "${NAME}"));
    }

    // ── JSON ────────────────────────────────────────────────────

    #[test]
    fn json_tells_a_key_from_a_value() {
        let tokens = kinds(Language::Json, r#"{"name": "maat", "n": 4, "ok": true}"#);
        assert_eq!(tokens[0], (TokenKind::Keyword, "\"name\"".into()));
        assert_eq!(tokens[1], (TokenKind::Str, "\"maat\"".into()));
        assert!(tokens.iter().any(|(k, t)| *k == TokenKind::Number && t == "4"));
        assert!(tokens.iter().any(|(k, t)| *k == TokenKind::Keyword && t == "true"));
    }

    #[test]
    fn json_reads_a_negative_and_exponent_number() {
        let tokens = kinds(Language::Json, r#"{"v": -1.5e10}"#);
        assert!(tokens.iter().any(|(k, t)| *k == TokenKind::Number && t == "-1.5e10"));
    }

    // ── Markdown ────────────────────────────────────────────────

    #[test]
    fn markdown_marks_headings_quotes_and_inline_code() {
        assert_eq!(kinds(Language::Markdown, "## Heading")[0].0, TokenKind::Keyword);
        assert_eq!(kinds(Language::Markdown, "> quoted")[0].0, TokenKind::Comment);
        let tokens = kinds(Language::Markdown, "use `cargo test` here");
        assert_eq!(tokens[0], (TokenKind::Str, "`cargo test`".into()));
    }

    #[test]
    fn a_fenced_block_stays_fenced_until_it_closes() {
        let (_, state) = Language::Markdown.lex("```rust", LineState::Normal);
        assert_eq!(state, LineState::FencedCode);

        let (tokens, state) = Language::Markdown.lex("## not a heading in here", state);
        assert_eq!(tokens[0].kind, TokenKind::Str, "code, not a heading");
        assert_eq!(state, LineState::FencedCode);

        let (_, state) = Language::Markdown.lex("```", state);
        assert_eq!(state, LineState::Normal);
    }

    // ── Incremental behaviour ───────────────────────────────────

    #[test]
    fn only_the_lines_up_to_the_one_asked_for_are_lexed() {
        let buffer = Buffer::from_text(&"let x = 1;\n".repeat(1000));
        let mut highlighter = Highlighter::new(Language::Rust);

        highlighter.tokens(&buffer, 10);
        assert_eq!(highlighter.cached_lines(), 11, "not the other 990");
    }

    #[test]
    fn invalidating_drops_only_what_comes_after() {
        let buffer = Buffer::from_text(&"let x = 1;\n".repeat(100));
        let mut highlighter = Highlighter::new(Language::Rust);
        highlighter.tokens(&buffer, 50);
        assert_eq!(highlighter.cached_lines(), 51);

        highlighter.invalidate_from(20);
        assert_eq!(highlighter.cached_lines(), 21, "everything above 20 survived");
    }

    #[test]
    fn a_block_comment_opened_far_above_still_colours_the_visible_line() {
        // The reason the state cache exists at all.
        let mut text = String::from("/* opens here\n");
        text.push_str(&"still inside\n".repeat(500));
        text.push_str("*/ closed\n");
        let buffer = Buffer::from_text(&text);

        let mut highlighter = Highlighter::new(Language::Rust);
        let tokens = highlighter.tokens(&buffer, 400);
        assert_eq!(tokens[0].kind, TokenKind::Comment, "line 400 is inside the comment");
    }

    #[test]
    fn changing_the_language_forgets_the_cache() {
        let buffer = Buffer::from_text(&"let x = 1;\n".repeat(50));
        let mut highlighter = Highlighter::new(Language::Rust);
        highlighter.tokens(&buffer, 30);

        highlighter.set_language(Language::Shell);
        assert_eq!(highlighter.cached_lines(), 1);
        assert_eq!(highlighter.language(), Language::Shell);
    }

    #[test]
    fn tokens_never_run_past_the_end_of_a_line() {
        // An unterminated string is the common way to walk off the end.
        for language in [Language::Rust, Language::Shell, Language::Json, Language::Toml] {
            for line in ["\"unterminated", "'also unterminated", "`", "x = \""] {
                let chars = line.chars().count();
                for token in language.lex(line, LineState::Normal).0 {
                    assert!(token.end <= chars, "{language:?} ran past the end of {line:?}");
                    assert!(token.start <= token.end, "{language:?} produced a reversed range");
                }
            }
        }
    }

    #[test]
    fn a_multibyte_line_produces_character_ranges_not_byte_ranges() {
        let line = r#"let café = "año";"#;
        let chars: Vec<char> = line.chars().collect();
        for token in Language::Rust.lex(line, LineState::Normal).0 {
            assert!(token.end <= chars.len(), "range is in characters, not bytes");
        }
        let tokens = kinds(Language::Rust, line);
        assert!(tokens.iter().any(|(k, t)| *k == TokenKind::Str && t == "\"año\""));
    }
}
