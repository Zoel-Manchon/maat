mod core;
mod ui;

use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::core::config::Config;
use crate::core::document::{DiskState, Document};
use crate::ui::app::App;

const USAGE: &str = "\
maat — a modal terminal editor with integrity awareness

USAGE:
    maat [OPTIONS] [FILE]

OPTIONS:
    --verify <FILE>   print the file's SHA-256 and its disk state, then exit
    -h, --help        show this help
    -V, --version     show the version

ENVIRONMENT:
    MAAT_AUDIT_LOG      append a structured event to this path on every save
    MAAT_AUDIT_FORMAT   `json` (default) or `cef`
    MAAT_CONFIG         config file to read instead of the usual locations
    MAAT_CLIPBOARD      1 to mirror yanks to the terminal clipboard (OSC 52)

CONFIG:
    $XDG_CONFIG_HOME/maat/config.toml, or ~/.config/maat/config.toml
    (%APPDATA%/maat/config.toml on Windows). See docs/config.example.toml.

EXIT CODES:
    0  clean exit
    1  an error occurred
    2  the edit was abandoned with unsaved changes (`:q!`)
";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    match arguments.first().map(String::as_str) {
        Some("-h") | Some("--help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("-V") | Some("--version") => {
            println!("maat {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        // Non-interactive integrity check: the same binary an appliance's boot
        // scripts can call to verify a config file without starting a UI.
        Some("--verify") => match arguments.get(1) {
            Some(path) => verify(Path::new(path)),
            None => {
                eprintln!("maat: --verify needs a file");
                ExitCode::from(1)
            }
        },
        _ => match edit(arguments.first().map(PathBuf::from)) {
            Ok(code) => code,
            Err(error) => {
                eprintln!("maat: {error}");
                ExitCode::from(1)
            }
        },
    }
}

/// Prints the hash of a file and whether it still matches what is on disk.
/// Machine-friendly: `<sha256>  <path>` on stdout, like `sha256sum`.
fn verify(path: &Path) -> ExitCode {
    match Document::open(path) {
        Ok(document) => {
            if document.disk_hash().is_none() {
                eprintln!("maat: {} does not exist", path.display());
                return ExitCode::from(2);
            }
            let mut stdout = io::stdout().lock();
            let _ = writeln!(stdout, "{}  {}", document.buffer_hash(), path.display());
            let state = match document.disk_state() {
                DiskState::Unchanged => "unchanged",
                DiskState::ModifiedExternally => "modified",
                DiskState::Missing => "missing",
                DiskState::NoFile => "absent",
            };
            let _ = writeln!(stdout, "state: {state}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("maat: cannot read {}: {error}", path.display());
            ExitCode::from(1)
        }
    }
}

/// The interactive path: set up the terminal, run the loop, always restore.
fn edit(path: Option<PathBuf>) -> io::Result<ExitCode> {
    let document = match &path {
        Some(path) => Document::open(path)?,
        None => Document::default(),
    };

    // Read before the first frame: the theme caches its colour decision on
    // first use, so the config has to get there first.
    let config = Config::load();
    if config.force_16_colour {
        ui::theme::force_16_colour();
    }

    let mut app = App::with_config(document, config.clone());
    if !config.unknown.is_empty() {
        // Not fatal, but not silent either: a typo in a key name should be
        // visible the moment the editor opens, not the day someone wonders why
        // their setting never worked.
        let first = config.unknown[0].clone();
        let extra = config.unknown.len() - 1;
        app.warn_about_config(&first, extra);
    }

    install_panic_hook();

    let mut terminal = setup_terminal()?;
    let outcome = run(&mut terminal, app);
    restore_terminal(&mut terminal)?;

    // Exit code 2 on an abandoned edit: `visudo`, `crontab -e` and friends
    // read it to know they must not install the file.
    outcome.map(|discarded| if discarded { ExitCode::from(2) } else { ExitCode::SUCCESS })
}

/// Raw mode + alternate screen: the editor takes over the whole terminal and
/// receives every keystroke before the shell can interpret it.
///
/// Bracketed paste is switched on here too, so a pasted block arrives as one
/// `Event::Paste` instead of being replayed key by key — which in Normal mode
/// would execute the paste as commands rather than insert it as text.
fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

/// ALWAYS undo it on the way out — otherwise the terminal is left unusable.
fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), DisableBracketedPaste, LeaveAlternateScreen)?;
    terminal.show_cursor()
}

/// Put the terminal back before a panic is printed.
///
/// A panic while in raw mode on the alternate screen leaves the user with a
/// terminal that echoes nothing and a message they cannot read — and, on the
/// appliance console this editor is aimed at, no second terminal to recover
/// from. The hook restores the screen first, then lets the default hook print
/// exactly what it would have printed.
///
/// It writes straight to stdout rather than through the `Terminal`, because by
/// the time a panic unwinds we cannot assume anything about who owns it.
fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
        default_hook(info);
    }));
}

/// The event loop: draw, wait for a key, apply it, repeat. Returns whether the
/// session ended by discarding unsaved changes.
///
/// `event::read()` **blocks**, so we never burn CPU doing nothing: there is no
/// 60 fps loop, we only redraw when something actually changed.
fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: App) -> io::Result<bool> {
    while !app.quit {
        terminal.draw(|frame| ui::render::draw(frame, &mut app))?;

        match event::read()? {
            // Windows emits both Press and Release for every key; without
            // this filter each keystroke would count twice.
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                app.handle_key(key);
                // The editor core never touches the terminal; anything it wants
                // said to the terminal — an OSC 52 clipboard copy — comes back
                // here to be written.
                if let Some(sequence) = app.take_terminal_output() {
                    let mut stdout = io::stdout();
                    let _ = stdout.write_all(sequence.as_bytes());
                    let _ = stdout.flush();
                }
            }
            // Arrives as one event thanks to bracketed paste: inserted as
            // text, never interpreted as commands.
            Event::Paste(text) => app.paste_text(&text),
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(app.discarded)
}
