//! Drawing the interface. Three stacked regions:
//!
//!   ┌──────────────────────────┐
//!   │ buffer + gutter          │  ← text, search matches, welcome screen
//!   ├──────────────────────────┤
//!   │ mode · file · metrics    │  ← status bar
//!   ├──────────────────────────┤
//!   │ :command, /search, msg   │  ← message line
//!   └──────────────────────────┘

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::core::mode::Mode;
use crate::ui::app::{App, Level};
use crate::ui::theme;

/// Gutter width for a buffer of `lines` lines.
fn gutter_width(lines: usize) -> u16 {
    let digits = lines.to_string().len().max(3);
    digits as u16 + 1
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

    if app.show_help {
        draw_help(frame);
    } else if !app.is_welcome() {
        place_cursor(frame, chunks[0], app, gutter);
    }
}

fn draw_buffer(frame: &mut Frame, area: Rect, app: &App, gutter: u16) {
    let height = area.height as usize;
    let width = area.width.saturating_sub(gutter) as usize;
    let total = app.document.buffer.line_count();
    let query = app.active_search();

    let mut lines: Vec<Line> = Vec::with_capacity(height);

    for screen_row in 0..height {
        let row = app.offset_row + screen_row;

        if row >= total {
            lines.push(Line::from(Span::styled("~", theme::tilde())));
            continue;
        }

        let shown_number = if app.relative_numbers && row != app.cursor.row {
            row.abs_diff(app.cursor.row)
        } else {
            row + 1
        };
        let number = format!(
            "{:>width$}│",
            shown_number,
            width = (gutter as usize).saturating_sub(1)
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

        let mut spans = vec![Span::styled(number, number_style)];
        spans.extend(highlight_matches(content, query, content_style));
        spans.push(Span::styled(
            " ".repeat(width.saturating_sub(content_len)),
            content_style,
        ));
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines).style(theme::text()), area);
}

fn highlight_matches(text: String, query: &str, base: Style) -> Vec<Span<'static>> {
    if query.is_empty() || !text.contains(query) {
        return vec![Span::styled(text, base)];
    }

    let mut spans = Vec::new();
    let mut start = 0;
    for (byte, matched) in text.match_indices(query) {
        if byte > start {
            spans.push(Span::styled(text[start..byte].to_string(), base));
        }
        spans.push(Span::styled(matched.to_string(), theme::search_match()));
        start = byte + matched.len();
    }
    if start < text.len() {
        spans.push(Span::styled(text[start..].to_string(), base));
    }
    spans
}

fn draw_welcome(frame: &mut Frame, area: Rect) {
    let height = area.height as usize;
    let top_padding = height.saturating_sub(14) / 2;
    let mut lines = vec![Line::from(""); top_padding];

    lines.extend([
        Line::from(Span::styled("M  A  A  T", theme::welcome_title())),
        Line::from(Span::styled("modal editor · integrity aware", theme::welcome_dim())),
        Line::from(""),
        Line::from(vec![
            Span::styled("i", theme::welcome_accent()),
            Span::styled("  start typing", theme::text()),
        ]),
        Line::from(vec![
            Span::styled(":w <path>", theme::welcome_accent()),
            Span::styled("  save the document", theme::text()),
        ]),
        Line::from(vec![
            Span::styled("/text", theme::welcome_accent()),
            Span::styled("  search the buffer", theme::text()),
        ]),
        Line::from(vec![
            Span::styled("?", theme::welcome_accent()),
            Span::styled("  open the key guide", theme::text()),
        ]),
        Line::from(""),
        Line::from(Span::styled("AS ABOVE, SO BELOW · SHA-256 WATCH", theme::welcome_dim())),
    ]);

    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .style(theme::text()),
        area,
    );
}

fn draw_status(frame: &mut Frame, area: Rect, app: &App) {
    let tag_style = match app.mode {
        Mode::Normal => theme::mode_tag(),
        Mode::Insert => theme::mode_tag_insert(),
        Mode::Command => theme::mode_tag_command(),
        Mode::Search => theme::mode_tag_search(),
    };
    let tag = format!(" {} ", app.mode.label());

    let name = app.document.name();
    let modified = if app.is_modified() { " [+]" } else { "" };
    let hash: String = app.hash().chars().take(10).collect();
    let position = format!(" {}:{} ", app.cursor.row + 1, app.cursor.col + 1);
    let (lines, words, chars) = app.stats();

    let left = vec![
        Span::styled(" MAAT ", theme::logo()),
        Span::styled(tag, tag_style),
        Span::styled(format!(" {name}"), theme::status()),
        Span::styled(modified, theme::modified()),
    ];

    let metrics = if area.width >= 96 {
        format!(" {lines}L {words}W {chars}C · sha256 {hash}…")
    } else if area.width >= 66 {
        format!(" {lines}L {words}W · {hash}…")
    } else {
        String::new()
    };
    let right_text = format!("{metrics}{position}");
    let used: usize = left.iter().map(|span| span.content.chars().count()).sum();
    let padding = (area.width as usize).saturating_sub(used + right_text.chars().count());

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

fn draw_help(frame: &mut Frame) {
    let area = centered_rect(frame.area(), 74, 22);
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Line::from(Span::styled(
            " MAAT · QUICK REFERENCE ",
            theme::overlay_title(),
        )))
        .border_style(theme::overlay_border())
        .style(theme::overlay());

    let lines = vec![
        help_line("MOVEMENT", "h j k l · w b · 0 $ · gg G"),
        help_line("INSERT", "i a I A · o O · Esc"),
        help_line("EDIT", "x · dd · yy · p P · u · Ctrl-r"),
        help_line("SEARCH", "/text · n next · N previous"),
        help_line("FILES", ":w · :w <path> · :w! · :q · :q! · :wq"),
        help_line("INTEGRITY", ":hash · :check · :info"),
        help_line("DISPLAY", ":set relativenumber · :set number"),
        Line::from(""),
        Line::from(vec![
            Span::styled("SHA-256 WATCH", theme::overlay_key()),
            Span::styled(
                "  Maat warns before overwriting external changes.",
                theme::overlay(),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Esc, ? or q to close",
            theme::overlay_dim(),
        )),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false })
            .style(theme::overlay()),
        area,
    );
}

fn help_line(key: &'static str, description: &'static str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!(" {key:<10}"), theme::overlay_key()),
        Span::styled(description, theme::overlay()),
    ])
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

    #[test]
    fn gutter_grows_with_the_line_count() {
        assert_eq!(gutter_width(1), 4);
        assert_eq!(gutter_width(999), 4);
        assert_eq!(gutter_width(1000), 5);
        assert_eq!(gutter_width(12345), 6);
    }

    #[test]
    fn search_highlighting_preserves_text() {
        let spans = highlight_matches("one two one".to_string(), "one", theme::text());
        let joined: String = spans.iter().map(|span| span.content.as_ref()).collect();
        assert_eq!(joined, "one two one");
    }
}
