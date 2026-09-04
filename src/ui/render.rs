//! Drawing the interface. Three stacked regions:
//!
//!   ┌──────────────────────────┐
//!   │ buffer + gutter          │  ← text, search matches, welcome screen
//!   ├──────────────────────────┤
//!   │ mode · file · metrics    │  ← status bar
//!   ├──────────────────────────┤
//!   │ :command, /search, msg   │  ← message line
//!   └──────────────────────────┘

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::core::mode::Mode;
use crate::core::syntax::TokenKind;
use crate::ui::app::{App, Level};
use crate::ui::theme;

/// Columns the gutter spends on decoration: a space either side of the rule.
const GUTTER_TRIM: u16 = 3;

/// Gutter width for a buffer of `lines` lines: `" 12 │ "`.
fn gutter_width(lines: usize) -> u16 {
    let digits = lines.to_string().len().max(3);
    digits as u16 + GUTTER_TRIM
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let gutter = gutter_width(app.document.buffer.line_count());
    let text_width = chunks[0].width.saturating_sub(gutter) as usize;
    let text_height = chunks[0].height as usize;

    app.ensure_visible(text_height, text_width.max(1));

    if app.is_welcome() {
        draw_welcome(frame, chunks[0]);
    } else {
        draw_buffer(frame, chunks[0], app, gutter);
    }
    draw_status(frame, chunks[1], app);
    draw_message(frame, chunks[2], app);

    if app.picker.is_some() {
        draw_picker(frame, app);
    } else if app.show_help {
        draw_help(frame);
    } else if !app.is_welcome() {
        place_cursor(frame, chunks[0], app, gutter);
    }
}

fn draw_buffer(frame: &mut Frame, area: Rect, app: &mut App, gutter: u16) {
    let height = area.height as usize;
    let width = area.width.saturating_sub(gutter) as usize;
    let total = app.document.buffer.line_count();
    // Owned up front: `tokens` needs `&mut` on the highlighter while the loop
    // reads the rest of `app`, and the query is a handful of characters.
    let query = app.active_search().to_string();
    // Same reason — the highlighter is taken out for the duration of the loop
    // and put back at the end, so the borrow checker sees two disjoint fields
    // instead of one overlapping borrow.
    let mut highlighter = std::mem::replace(
        &mut app.highlighter,
        crate::core::syntax::Highlighter::new(crate::core::syntax::Language::PlainText),
    );

    let mut lines: Vec<Line> = Vec::with_capacity(height);

    for screen_row in 0..height {
        let row = app.offset_row + screen_row;

        if row >= total {
            // Past the last line: the tilde sits in the number column, and the
            // rule stops, so where the buffer ends stays unambiguous.
            let filler = format!(
                "{:>width$}",
                "~",
                width = (gutter as usize).saturating_sub(GUTTER_TRIM as usize)
            );
            lines.push(Line::from(Span::styled(filler, theme::tilde())));
            continue;
        }

        let shown_number = if app.relative_numbers && row != app.cursor.row {
            row.abs_diff(app.cursor.row)
        } else {
            row + 1
        };
        let number = format!(
            "{:>width$} │ ",
            shown_number,
            width = (gutter as usize).saturating_sub(GUTTER_TRIM as usize)
        );
        let is_current = row == app.cursor.row;
        let number_style = if is_current {
            theme::gutter_current()
        } else {
            theme::gutter()
        };
        let content_style = if is_current {
            theme::current_line()
        } else {
            theme::text()
        };

        let content: String = app
            .document
            .buffer
            .line(row)
            .unwrap_or("")
            .chars()
            .skip(app.offset_col)
            .take(width)
            .collect();
        let content_len = content.chars().count();

        // Where the visual selection falls on this row, in the coordinates of
        // the slice actually on screen.
        let selection = app.selection().and_then(|(start, end)| {
            if row < start.0 || row > end.0 {
                return None;
            }
            let line_len = app.document.buffer.line_len(row);
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 { end.1.saturating_add(1).min(line_len) } else { line_len };
            let from = from.saturating_sub(app.offset_col).min(width);
            let to = to.saturating_sub(app.offset_col).min(width);
            (to > from).then_some((from, to))
        });

        // Syntax first, then search, then selection — in that order, because
        // each is more urgent than the one before it. The selection is what
        // the next keystroke acts on, so nothing may hide it.
        let tokens = highlighter.tokens(&app.document.buffer, row);
        let mut spans = vec![Span::styled(number, number_style)];
        spans.extend(style_line(
            &content,
            content_style,
            &tokens,
            app.offset_col,
            &query,
            selection,
        ));
        spans.push(Span::styled(
            " ".repeat(width.saturating_sub(content_len)),
            content_style,
        ));
        lines.push(Line::from(spans));
    }

    app.highlighter = highlighter;
    frame.render_widget(Paragraph::new(lines).style(theme::text()), area);
}

/// Styles one visible line by painting three layers onto a per-character
/// buffer, then coalescing equal neighbours into spans.
///
/// Nesting the three passes as functions — syntax calling search calling
/// selection — is where this kind of code usually ends up, and it goes wrong
/// the moment two of them overlap on the same character: whichever ran first
/// has already cut the string, and the second cannot reach inside it. Painting
/// per character makes the precedence explicit and the overlaps trivial.
///
/// `text` is the slice already on screen; `offset` is the column it starts at,
/// which is what maps the token ranges (whole-line) onto it.
fn style_line(
    text: &str,
    base: Style,
    tokens: &[crate::core::syntax::Token],
    offset: usize,
    query: &str,
    selection: Option<(usize, usize)>,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let mut styles = vec![base; chars.len()];

    // 1. Syntax, shifted from line coordinates into the visible window.
    for token in tokens {
        let style = match token.kind {
            TokenKind::Keyword => theme::syntax_keyword(),
            TokenKind::Str => theme::syntax_string(),
            TokenKind::Comment => theme::syntax_comment(),
            TokenKind::Number => theme::syntax_number(),
        };
        // Keep the line background (the current-line highlight) and change
        // only the foreground, or a token would punch a hole in it.
        let style = match base.bg {
            Some(bg) => style.bg(bg),
            None => style,
        };
        let from = token.start.saturating_sub(offset);
        let to = token.end.saturating_sub(offset);
        for slot in styles.iter_mut().take(to.min(chars.len())).skip(from) {
            *slot = style;
        }
    }

    // 2. Search matches.
    if !query.is_empty() {
        let query_len = query.chars().count();
        for (byte, matched) in text.match_indices(query) {
            let start = text[..byte].chars().count();
            for slot in styles.iter_mut().skip(start).take(matched.chars().count().max(query_len)) {
                *slot = theme::search_match();
            }
        }
    }

    // 3. The selection, which nothing may hide.
    if let Some((from, to)) = selection {
        for slot in styles.iter_mut().take(to.min(chars.len())).skip(from.min(chars.len())) {
            *slot = theme::selection();
        }
    }

    // Coalesce: one span per run of equal style, so the terminal is not asked
    // to draw a span per character.
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut run_style = styles[0];
    for (index, ch) in chars.iter().enumerate() {
        if styles[index] != run_style {
            spans.push(Span::styled(std::mem::take(&mut run), run_style));
            run_style = styles[index];
        }
        run.push(*ch);
    }
    if !run.is_empty() {
        spans.push(Span::styled(run, run_style));
    }
    spans
}

/// Key/description pairs offered on the empty-buffer screen.
const WELCOME_HINTS: [(&str, &str); 4] = [
    ("i", "start typing"),
    (":w <path>", "save the document"),
    ("/text", "search the buffer"),
    ("?", "open the key guide"),
];

const WELCOME_TITLE: &str = "M  A  A  T";
const WELCOME_SUBTITLE: &str = "modal editor · integrity aware";
const WELCOME_FOOTER: &str = "AS ABOVE, SO BELOW · SHA-256 WATCH";
/// Blank columns between the key column and the description column.
const WELCOME_GAP: usize = 3;

fn draw_welcome(frame: &mut Frame, area: Rect) {
    let width = |text: &str| text.chars().count();

    // The hints are a two-column block: keys right-aligned against the gap,
    // descriptions left-aligned after it. Centring each line independently —
    // which is what `Alignment::Center` does — leaves the key column ragged.
    let key_column = WELCOME_HINTS.iter().map(|(key, _)| width(key)).max().unwrap_or(0);
    let description_column = WELCOME_HINTS
        .iter()
        .map(|(_, description)| width(description))
        .max()
        .unwrap_or(0);
    let hints_width = key_column + WELCOME_GAP + description_column;

    // Every element is centred against one block, so the whole screen shares a
    // single optical axis instead of three competing ones.
    let block_width = hints_width
        .max(width(WELCOME_TITLE))
        .max(width(WELCOME_SUBTITLE))
        .max(width(WELCOME_FOOTER))
        .min(area.width as usize);
    let left_margin = (area.width as usize).saturating_sub(block_width) / 2;
    let pad = " ".repeat(left_margin);

    // Centres `text` inside the block, then shifts it out to the block's margin.
    let centred = |text: &str, style| {
        let inner = block_width.saturating_sub(width(text)) / 2;
        Line::from(Span::styled(
            format!("{pad}{}{text}", " ".repeat(inner)),
            style,
        ))
    };

    let hints_indent = pad.clone() + &" ".repeat(block_width.saturating_sub(hints_width) / 2);
    let hint = |key: &str, description: &str| {
        Line::from(vec![
            Span::styled(
                format!("{hints_indent}{key:>key_column$}"),
                theme::welcome_accent(),
            ),
            Span::styled(
                format!("{}{description}", " ".repeat(WELCOME_GAP)),
                theme::text(),
            ),
        ])
    };

    let mut body = vec![
        centred(WELCOME_TITLE, theme::welcome_title()),
        centred(WELCOME_SUBTITLE, theme::welcome_dim()),
        Line::from(""),
    ];
    body.extend(
        WELCOME_HINTS
            .iter()
            .map(|(key, description)| hint(key, description)),
    );
    body.push(Line::from(""));
    body.push(centred(WELCOME_FOOTER, theme::welcome_dim()));

    // Centred against the real line count; a hardcoded guess drifts the moment
    // a hint is added or removed.
    let top_padding = (area.height as usize).saturating_sub(body.len()) / 2;
    let mut lines = vec![Line::from(""); top_padding];
    lines.extend(body);

    frame.render_widget(Paragraph::new(lines).style(theme::text()), area);
}

/// Columns the status bar always keeps for the file name before it starts
/// dropping metrics.
const NAME_MIN_WIDTH: usize = 12;

/// Shortens `text` to `limit` columns, marking the cut with an ellipsis.
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    match limit.checked_sub(1) {
        Some(room) => text.chars().take(room).chain(['…']).collect(),
        None => String::new(),
    }
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let tag_style = match app.mode {
        Mode::Normal => theme::mode_tag(),
        Mode::Insert => theme::mode_tag_insert(),
        Mode::Command => theme::mode_tag_command(),
        Mode::Search => theme::mode_tag_search(),
        Mode::Visual | Mode::VisualLine => theme::mode_tag_visual(),
    };
    let tag = format!(" {} ", app.mode.label());

    // The buffer counter only appears once there is more than one: a status
    // bar that says [1/1] on every single-file session is noise.
    let name = if app.buffer_count() > 1 {
        format!("{} [{}/{}]", app.document.name(), app.buffer_index(), app.buffer_count())
    } else {
        app.document.name()
    };
    let modified = if app.is_modified() { " [+]" } else { "" };
    let hash: String = app.hash().chars().take(10).collect();
    // A count in progress is echoed next to the position, the way Vim shows it
    // in the bottom right: typing `12` and then walking away should not leave
    // the next keystroke doing something twelve times without warning.
    let position = match app.pending_count() {
        Some(count) => format!(" {count} · {}:{} ", app.cursor.row + 1, app.cursor.col + 1),
        None => format!(" {}:{} ", app.cursor.row + 1, app.cursor.col + 1),
    };
    let (lines, words, chars) = app.stats();

    let mut left = vec![
        Span::styled(" MAAT ", theme::logo()),
        Span::styled(tag, tag_style),
        Span::styled(format!(" {name}"), theme::status()),
        Span::styled(modified, theme::modified()),
    ];

    // Width breakpoints guess; measuring does not. Take the richest metrics
    // block that still leaves the file name readable.
    let fixed: usize = left.iter().map(|span| span.content.chars().count()).sum::<usize>()
        - name.chars().count()
        + position.chars().count();
    let metrics = [
        format!(" {lines}L {words}W {chars}C · sha256 {hash}…"),
        format!(" {lines}L {words}W · {hash}…"),
        format!(" {lines}L {words}W"),
        String::new(),
    ]
    .into_iter()
    .find(|candidate| fixed + candidate.chars().count() + NAME_MIN_WIDTH <= area.width as usize)
    .unwrap_or_default();

    // The name is the only elastic field, so it is the one that yields.
    let name_budget = (area.width as usize).saturating_sub(fixed + metrics.chars().count());
    left[2] = Span::styled(format!(" {}", truncate(&name, name_budget)), theme::status());

    let used: usize = left.iter().map(|span| span.content.chars().count()).sum();
    let padding = (area.width as usize)
        .saturating_sub(used + metrics.chars().count() + position.chars().count());

    let mut spans = left;
    spans.push(Span::styled(" ".repeat(padding), theme::status()));
    if !metrics.is_empty() {
        spans.push(Span::styled(metrics, theme::hash()));
    }
    spans.push(Span::styled(position, theme::status_dim()));

    frame.render_widget(Paragraph::new(Line::from(spans)).style(theme::status()), area);
}

fn draw_message(frame: &mut Frame, area: Rect, app: &App) {
    let (text, style) = match app.mode {
        Mode::Command => (format!(":{}", app.command), theme::message()),
        Mode::Search => (format!("/{}", app.search), theme::message()),
        _ => {
            let style = match app.level {
                Level::Info => theme::message(),
                Level::Warn => theme::message_warn(),
                Level::Error => theme::message_error(),
            };
            (app.message.clone(), style)
        }
    };

    frame.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
}

/// The key guide, as section/keys pairs. An empty section opens a free line.
const HELP_ROWS: [(&str, &str); 19] = [
    ("MOVEMENT", "h j k l · w b · 0 $ · gg G · 12G"),
    ("COUNTS", "3j · 5x · 2dd · 3p — before any of the above"),
    ("INSERT", "i a I A · o O · Esc"),
    ("EDIT", "x · dd · yy · cc · p P · u · Ctrl-r"),
    ("OPERATORS", "d y c + motion: dw · c3w · d$ · dG · dgg"),
    ("VISUAL", "v char · V line · o other end · Esc"),
    ("V-OPS", "d delete · y yank · c change · p replace"),
    ("SEARCH", "/text · n next · N previous"),
    ("REPLACE", ":s/old/new/ · :s/old/new/g · :%s/old/new/g"),
    ("FILES", ":w · :w <path> · :w! · :q · :q! · :wq"),
    ("BUFFERS", ":e <path> · :bn · :bp · :bd · :ls"),
    ("PICKER", "Ctrl-p find file · :buffers · type to filter"),
    ("INTEGRITY", ":hash · :check · :info"),
    ("RECOVERY", ":recover · :discard"),
    ("DISPLAY", ":set relativenumber · :set number"),
    ("SYNTAX", "detected from the file · :set syntax · :set nosyntax"),
    ("", ""),
    ("CONFIG", "~/.config/maat/config.toml · [keys] rebinds"),
    ("CLOSE", "Esc · ? · q"),
];

const HELP_TITLE: &str = " MAAT · QUICK REFERENCE ";
/// Blank columns left of the key column, right of the box, and between columns.
const HELP_PADDING: usize = 2;

/// The picker overlay: a filter line and the matching entries.
///
/// Sized to the screen rather than to its contents — a file list is unbounded,
/// and a box that grows with it would run off the terminal on the first large
/// repository.
fn draw_picker(frame: &mut Frame, app: &App) {
    let Some(picker) = app.picker.as_ref() else { return };

    let area = frame.area();
    let box_width = (area.width as usize).saturating_sub(8).clamp(20, 76);
    let matches = picker.matches();
    // Leave room for the border, the query line and the count.
    let rows = (area.height as usize).saturating_sub(8).clamp(1, 14);
    let visible = matches.len().min(rows);

    // Scroll the window so the highlighted row is always inside it.
    let first = picker.selected.saturating_sub(visible.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::with_capacity(visible + 2);
    lines.push(Line::from(vec![
        Span::styled("  ", theme::overlay_dim()),
        Span::styled("> ", theme::welcome_accent()),
        Span::styled(picker.query.clone(), theme::overlay_dim()),
        Span::styled("_", theme::welcome_accent()),
    ]));
    lines.push(Line::from(Span::styled("", theme::overlay_dim())));

    for (offset, (_, entry)) in matches.iter().skip(first).take(visible).enumerate() {
        let index = first + offset;
        let selected = index == picker.selected;
        let marker = if selected { "  ▸ " } else { "    " };
        let style = if selected { theme::selection() } else { theme::overlay_dim() };
        let label = truncate(&entry.label, box_width.saturating_sub(6));
        lines.push(Line::from(Span::styled(format!("{marker}{label}"), style)));
    }

    if matches.is_empty() {
        lines.push(Line::from(Span::styled("    no match", theme::tilde())));
    }

    let box_height = lines.len() + 2;
    let x = area.x + (area.width.saturating_sub(box_width as u16)) / 2;
    let y = area.y + (area.height.saturating_sub(box_height as u16)) / 3;
    let rect = Rect {
        x,
        y,
        width: (box_width as u16).min(area.width),
        height: (box_height as u16).min(area.height),
    };

    let title = format!(" {} · {} of {} ", picker.title, matches.len(), picker.entry_count());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::overlay_border())
        .style(theme::overlay())
        .title(Line::from(Span::styled(title, theme::overlay_title())));

    frame.render_widget(Clear, rect);
    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

fn draw_help(frame: &mut Frame) {
    let width = |text: &str| text.chars().count();
    let key_column = HELP_ROWS.iter().map(|(key, _)| width(key)).max().unwrap_or(0);
    let value_column = HELP_ROWS.iter().map(|(_, value)| width(value)).max().unwrap_or(0);

    // Sizing the box to its contents; a fixed 74x22 left nine dead rows and a
    // trailing void on an 80x25 console.
    let content_width = HELP_PADDING + key_column + HELP_PADDING + value_column + HELP_PADDING;
    let box_width = content_width.max(width(HELP_TITLE) + HELP_PADDING) + 2;
    // One blank row inside each border, plus the borders themselves.
    let box_height = HELP_ROWS.len() + 2 + 2;

    let area = centered_rect(frame.area(), box_width as u16, box_height as u16);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(Span::styled(HELP_TITLE, theme::overlay_title())))
        .border_style(theme::overlay_border())
        .style(theme::overlay());

    let indent = " ".repeat(HELP_PADDING);
    let gap = " ".repeat(HELP_PADDING);
    // Whatever the box actually got, minus borders, indent, key column and gap.
    // Values are truncated rather than wrapped: wrapping folds a long entry onto
    // column zero of the next row and destroys the two-column alignment.
    let value_budget = (area.width as usize)
        .saturating_sub(2 + HELP_PADDING + key_column + HELP_PADDING + HELP_PADDING);

    let mut lines = vec![Line::from("")];
    lines.extend(HELP_ROWS.iter().map(|(key, value)| {
        if key.is_empty() {
            return Line::from("");
        }
        let style = if *key == "CLOSE" {
            theme::overlay_dim()
        } else {
            theme::overlay()
        };
        Line::from(vec![
            Span::styled(format!("{indent}{key:<key_column$}"), theme::overlay_key()),
            Span::styled(format!("{gap}{}", truncate(value, value_budget)), style),
        ])
    }));
    lines.push(Line::from(""));

    frame.render_widget(Paragraph::new(lines).block(block).style(theme::overlay()), area);
}

fn centered_rect(parent: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = parent.width.saturating_sub(4).min(max_width).max(1);
    let height = parent.height.saturating_sub(2).min(max_height).max(1);
    Rect {
        x: parent.x + parent.width.saturating_sub(width) / 2,
        y: parent.y + parent.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn place_cursor(frame: &mut Frame, area: Rect, app: &App, gutter: u16) {
    if matches!(app.mode, Mode::Command | Mode::Search) {
        let input_len = if app.mode == Mode::Command {
            app.command.chars().count()
        } else {
            app.search.chars().count()
        };
        let x = 1 + input_len as u16;
        let y = frame.area().height.saturating_sub(1);
        frame.set_cursor_position((x.min(frame.area().width.saturating_sub(1)), y));
        return;
    }

    let x = area.x + gutter + (app.cursor.col.saturating_sub(app.offset_col)) as u16;
    let y = area.y + (app.cursor.row.saturating_sub(app.offset_row)) as u16;
    frame.set_cursor_position((
        x.min(area.right().saturating_sub(1)),
        y.min(area.bottom().saturating_sub(1)),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::document::Document;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    /// Renders `app` on an 80x25 console — the size of the Linux VGA text
    /// console Maat actually ships on — and returns the frame as plain text.
    fn render_at(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                let row: String = (0..width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect();
                format!("|{}|", row.trim_end())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The foreground colour of every cell on one rendered row.
    fn row_colours(app: &mut App, width: u16, height: u16, row: u16) -> Vec<(char, Color)> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..width)
            .map(|x| {
                let cell = buffer.cell((x, row)).unwrap();
                (cell.symbol().chars().next().unwrap_or(' '), cell.fg)
            })
            .collect()
    }

    #[test]
    fn a_rust_file_is_coloured_on_screen() {
        // The end-to-end check: a real document, through the real draw path,
        // read back off the rendered cells.
        let mut app = App::new(Document::from_text_for_test(
            "let x = 42; // note\n",
        ));
        app.set_language_for_test(crate::core::syntax::Language::Rust);

        let cells = row_colours(&mut app, 80, 25, 0);
        let text: String = cells.iter().map(|(ch, _)| *ch).collect();
        // Character index, not byte: the gutter's `│` is three bytes wide, and
        // `str::find` would put every lookup after it three cells to the left.
        let at = |needle: &str| {
            let byte = text.find(needle).expect("present on the row");
            text[..byte].chars().count()
        };

        let keyword = cells[at("let")].1;
        let number = cells[at("42")].1;
        let comment = cells[at("// note")].1;
        let plain = cells[at(" x ") + 1].1;

        assert_eq!(Some(keyword), theme::syntax_keyword().fg, "`let` is a keyword");
        assert_eq!(Some(number), theme::syntax_number().fg, "`42` is a number");
        assert_eq!(Some(comment), theme::syntax_comment().fg, "the trailing comment");
        assert_ne!(plain, keyword, "ordinary text is not coloured as a keyword");
    }

    #[test]
    fn an_unknown_file_type_renders_in_one_colour() {
        let mut app = App::new(Document::from_text_for_test("let x = 42; // note\n"));
        // PlainText is the default for a buffer with no path.
        let cells = row_colours(&mut app, 80, 25, 0);
        let text: String = cells.iter().map(|(ch, _)| *ch).collect();
        let start = text[..text.find("let").unwrap()].chars().count();

        let colours: std::collections::HashSet<_> =
            cells[start..start + 19].iter().map(|(_, fg)| *fg).collect();
        assert_eq!(colours.len(), 1, "no highlighting without a known language");
    }

    /// Row indices of the frame that carry any ink.
    fn occupied_rows(frame: &str) -> Vec<usize> {
        frame
            .lines()
            .enumerate()
            .filter(|(_, row)| !row.trim_matches('|').trim().is_empty())
            .map(|(index, _)| index)
            .collect()
    }

    #[test]
    fn the_welcome_screen_is_vertically_centred() {
        let mut app = App::new(Document::from_text_for_test(""));
        let frame = render_at(&mut app, 80, 25);
        // The last two rows are the status and message bars.
        let rows = occupied_rows(&frame);
        let (first, last) = (rows[0], *rows.iter().filter(|row| **row < 23).max().unwrap());
        let above = first;
        let below = 23 - last - 1;
        assert!(
            above.abs_diff(below) <= 1,
            "welcome is off-centre: {above} rows above, {below} below\n{frame}"
        );
    }

    #[test]
    fn the_welcome_keys_share_one_column() {
        let mut app = App::new(Document::from_text_for_test(""));
        let frame = render_at(&mut app, 80, 25);
        // Each hint's description starts at the same column, so the keys read as
        // a column rather than four independently centred lines.
        let columns: Vec<usize> = frame
            .lines()
            .filter_map(|row| row.find("   s").or_else(|| row.find("   o")))
            .collect();
        assert_eq!(columns.len(), 4, "expected four hints\n{frame}");
        assert!(
            columns.iter().all(|column| *column == columns[0]),
            "hint descriptions are ragged: {columns:?}\n{frame}"
        );
    }

    #[test]
    fn the_help_overlay_has_no_dead_rows() {
        let mut app = App::new(Document::from_text_for_test(""));
        app.show_help = true;
        let frame = render_at(&mut app, 80, 25);
        let row_of = |needle: char| {
            frame
                .lines()
                .position(|row| row.contains(needle))
                .unwrap_or_else(|| panic!("no {needle} border\n{frame}"))
        };
        // The box is exactly its contents plus one breathing row inside each
        // border; a hardcoded height used to leave nine empty rows at the foot.
        assert_eq!(
            row_of('└') - row_of('┌'),
            HELP_ROWS.len() + 3,
            "help overlay is not sized to its contents\n{frame}"
        );
    }

    #[test]
    fn the_status_bar_never_overflows_its_width() {
        let long = "a_very_long_generated_filename_indeed.rs";
        for width in [40u16, 46, 60, 80, 120] {
            let mut app = App::new(Document::from_text_for_test("hello\nworld"));
            app.message = long.to_string();
            let frame = render_at(&mut app, width, 25);
            for row in frame.lines() {
                let printed = row.trim_matches('|').chars().count();
                assert!(
                    printed <= width as usize,
                    "row of {printed} cols exceeds {width}\n{frame}"
                );
            }
        }
    }

    #[test]
    fn narrow_terminals_keep_the_help_columns_aligned() {
        let mut app = App::new(Document::from_text_for_test(""));
        app.show_help = true;
        let frame = render_at(&mut app, 46, 12);
        // Truncation, not wrapping: nothing may spill onto a row of its own.
        let starts: Vec<usize> = frame
            .lines()
            .filter(|row| row.contains("MOVEMENT") || row.contains("INSERT") || row.contains("EDIT"))
            .filter_map(|row| row.find(|c: char| c.is_ascii_uppercase()))
            .collect();
        assert_eq!(starts.len(), 3, "expected three key rows\n{frame}");
        assert!(
            starts.iter().all(|start| *start == starts[0]),
            "key column drifts when narrow: {starts:?}\n{frame}"
        );
    }

    #[test]
    fn gutter_grows_with_the_line_count() {
        assert_eq!(gutter_width(1), 6);
        assert_eq!(gutter_width(999), 6);
        assert_eq!(gutter_width(1000), 7);
        assert_eq!(gutter_width(12345), 8);
    }

    /// The property every styling layer has to keep: colours change, the text
    /// on screen does not.
    fn styled_text(
        text: &str,
        tokens: &[crate::core::syntax::Token],
        query: &str,
        selection: Option<(usize, usize)>,
    ) -> String {
        style_line(text, theme::text(), tokens, 0, query, selection)
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn search_highlighting_preserves_text() {
        assert_eq!(styled_text("one two one", &[], "one", None), "one two one");
    }

    #[test]
    fn every_layer_preserves_the_text_it_colours() {
        use crate::core::syntax::{Language, LineState};
        let line = r#"let x = "one"; // one"#;
        let tokens = Language::Rust.lex(line, LineState::Normal).0;

        assert_eq!(styled_text(line, &tokens, "", None), line, "syntax alone");
        assert_eq!(styled_text(line, &tokens, "one", None), line, "syntax + search");
        assert_eq!(
            styled_text(line, &tokens, "one", Some((4, 9))),
            line,
            "syntax + search + selection, all overlapping"
        );
    }

    #[test]
    fn the_selection_wins_over_the_layers_beneath_it() {
        use crate::core::syntax::{Language, LineState};
        let line = "let x = 1;";
        let tokens = Language::Rust.lex(line, LineState::Normal).0;

        // `let` is a keyword and also inside the selection: it must be drawn
        // as selected, because that is what the next keystroke acts on.
        let spans = style_line(line, theme::text(), &tokens, 0, "", Some((0, 3)));
        assert_eq!(spans[0].style, theme::selection());
        assert_eq!(spans[0].content.as_ref(), "let");
    }

    #[test]
    fn a_token_keeps_the_line_background_under_it() {
        use crate::core::syntax::{Language, LineState};
        let line = "let x = 1;";
        let tokens = Language::Rust.lex(line, LineState::Normal).0;

        // On the cursor's line the background is the current-line highlight;
        // a keyword must not punch a hole in it.
        let spans = style_line(line, theme::current_line(), &tokens, 0, "", None);
        assert_eq!(spans[0].style.bg, theme::current_line().bg);
        assert_eq!(spans[0].style.fg, theme::syntax_keyword().fg);
    }

    #[test]
    fn an_empty_line_produces_no_spans() {
        assert!(style_line("", theme::text(), &[], 0, "", None).is_empty());
    }

    #[test]
    fn a_horizontally_scrolled_line_shifts_the_tokens_with_it() {
        use crate::core::syntax::{Language, LineState};
        let line = "let value = 12345;";
        let tokens = Language::Rust.lex(line, LineState::Normal).0;

        // Show the line from column 12: the number starts at 12, so the very
        // first character on screen must already be coloured as one.
        let visible: String = line.chars().skip(12).collect();
        let spans = style_line(&visible, theme::text(), &tokens, 12, "", None);
        assert_eq!(spans[0].style.fg, theme::syntax_number().fg);
    }
}
