//! "Oxblood" theme: ink, wine and bone — an old book rendered in a terminal.
//!
//! Deliberately not amber: that identity belongs to `phosphor`, its sibling
//! project. Every colour lives here; no other module should ever hard-code one.
//!
//! Appliance consoles often expose only 16 ANSI colours. When true colour is
//! unavailable (`COLORTERM` unset), every constant degrades to its nearest
//! indexed colour instead of rendering as mud — see [`truecolor`].

use ratatui::style::{Color, Modifier, Style};
use std::sync::OnceLock;

// ── Palette ─────────────────────────────────────────────────────
/// True colour is available unless the terminal says otherwise. Computed once:
/// reading the environment on every frame would be wasteful and pointless.
fn truecolor() -> bool {
    static TRUECOLOR: OnceLock<bool> = OnceLock::new();
    *TRUECOLOR.get_or_init(|| {
        if FORCE_16_COLOUR.get().copied().unwrap_or(false) {
            return false;
        }
        std::env::var("COLORTERM")
            .map(|value| value.contains("truecolor") || value.contains("24bit"))
            .unwrap_or(false)
    })
}

/// Set from the config before anything is drawn.
///
/// `COLORTERM` lies in both directions — a multiplexer that strips it, a
/// terminal that claims true colour and renders mud — so the config gets the
/// last word. It has to be set before the first `truecolor()` call, because
/// that answer is cached for the life of the process.
static FORCE_16_COLOUR: OnceLock<bool> = OnceLock::new();

pub fn force_16_colour() {
    let _ = FORCE_16_COLOUR.set(true);
}

/// Picks the 24-bit colour or its indexed fallback, depending on the terminal.
fn pick(r: u8, g: u8, b: u8, fallback: Color) -> Color {
    if truecolor() {
        Color::Rgb(r, g, b)
    } else {
        fallback
    }
}

// Ink and panels
pub fn bg() -> Color { pick(0x14, 0x09, 0x0b, Color::Black) }
pub fn bg_panel() -> Color { pick(0x22, 0x11, 0x14, Color::Black) }
pub fn bg_current() -> Color { pick(0x1f, 0x0f, 0x12, Color::Black) }
pub fn bg_overlay() -> Color { pick(0x1a, 0x0c, 0x0f, Color::Black) }

// Bone and ivory
pub fn fg() -> Color { pick(0xe8, 0xd8, 0xce, Color::White) }
pub fn fg_bright() -> Color { pick(0xf5, 0xed, 0xe6, Color::LightYellow) }
pub fn dim() -> Color { pick(0x9a, 0x70, 0x70, Color::DarkGray) }
pub fn faint() -> Color { pick(0x3a, 0x22, 0x25, Color::DarkGray) }

// Wine, rose and gold
pub fn wine() -> Color { pick(0xb0, 0x3a, 0x48, Color::Red) }
pub fn rose() -> Color { pick(0xd9, 0x6a, 0x78, Color::LightRed) }
pub fn gold() -> Color { pick(0xe0, 0xa4, 0x58, Color::Yellow) }
pub fn error_colour() -> Color { pick(0xf0, 0x5d, 0x6c, Color::LightRed) }
pub fn hash_colour() -> Color { pick(0xc4, 0x8b, 0x94, Color::LightMagenta) }

pub fn text() -> Style {
    Style::default().fg(fg()).bg(bg())
}

pub fn current_line() -> Style {
    Style::default().fg(fg_bright()).bg(bg_current())
}

pub fn gutter() -> Style {
    Style::default().fg(dim()).bg(bg())
}

pub fn gutter_current() -> Style {
    Style::default()
        .fg(gold())
        .bg(bg_current())
        .add_modifier(Modifier::BOLD)
}

pub fn tilde() -> Style {
    Style::default().fg(faint()).bg(bg())
}

pub fn logo() -> Style {
    Style::default()
        .fg(fg_bright())
        .bg(bg_panel())
        .add_modifier(Modifier::BOLD)
}

pub fn status() -> Style {
    Style::default().fg(fg()).bg(bg_panel())
}

pub fn mode_tag() -> Style {
    Style::default()
        .fg(bg())
        .bg(wine())
        .add_modifier(Modifier::BOLD)
}

pub fn mode_tag_insert() -> Style {
    Style::default()
        .fg(bg())
        .bg(fg_bright())
        .add_modifier(Modifier::BOLD)
}

pub fn mode_tag_command() -> Style {
    Style::default()
        .fg(bg())
        .bg(rose())
        .add_modifier(Modifier::BOLD)
}

pub fn mode_tag_search() -> Style {
    Style::default()
        .fg(bg())
        .bg(gold())
        .add_modifier(Modifier::BOLD)
}

pub fn mode_tag_visual() -> Style {
    Style::default()
        .fg(bg())
        .bg(rose())
        .add_modifier(Modifier::BOLD)
}

/// The highlighted span in visual mode. Deliberately not the search style: on
/// screen it has to be obvious which one you are looking at when a selection
/// happens to sit on top of a match.
pub fn selection() -> Style {
    Style::default()
        .fg(bg())
        .bg(rose())
        .add_modifier(Modifier::BOLD)
}

pub fn hash() -> Style {
    Style::default().fg(hash_colour()).bg(bg_panel())
}

pub fn status_dim() -> Style {
    Style::default().fg(dim()).bg(bg_panel())
}

pub fn modified() -> Style {
    Style::default()
        .fg(gold())
        .bg(bg_panel())
        .add_modifier(Modifier::BOLD)
}

pub fn search_match() -> Style {
    Style::default()
        .fg(bg())
        .bg(gold())
        .add_modifier(Modifier::BOLD)
}

pub fn message() -> Style {
    Style::default().fg(fg()).bg(bg())
}

pub fn message_warn() -> Style {
    Style::default()
        .fg(gold())
        .bg(bg())
        .add_modifier(Modifier::BOLD)
}

pub fn message_error() -> Style {
    Style::default()
        .fg(error_colour())
        .bg(bg())
        .add_modifier(Modifier::BOLD)
}

pub fn welcome_title() -> Style {
    Style::default()
        .fg(fg_bright())
        .bg(bg())
        .add_modifier(Modifier::BOLD)
}

pub fn welcome_accent() -> Style {
    Style::default()
        .fg(gold())
        .bg(bg())
        .add_modifier(Modifier::BOLD)
}

pub fn welcome_dim() -> Style {
    Style::default().fg(dim()).bg(bg())
}

pub fn overlay() -> Style {
    Style::default().fg(fg()).bg(bg_overlay())
}

pub fn overlay_border() -> Style {
    Style::default().fg(wine()).bg(bg_overlay())
}

pub fn overlay_title() -> Style {
    Style::default()
        .fg(fg_bright())
        .bg(bg_overlay())
        .add_modifier(Modifier::BOLD)
}

pub fn overlay_key() -> Style {
    Style::default()
        .fg(gold())
        .bg(bg_overlay())
        .add_modifier(Modifier::BOLD)
}

pub fn overlay_dim() -> Style {
    Style::default().fg(dim()).bg(bg_overlay())
}
