mod core;
mod ui;

use crossterm::event::{self, Event, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Stdout, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

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

    let mut terminal = setup_terminal()?;
    let outcome = run(&mut terminal, App::new(document));
    restore_terminal(&mut terminal)?;

    // Exit code 2 on an abandoned edit: `visudo`, `crontab -e` and friends
    // read it to know they must not install the file.
    outcome.map(|discarded| if discarded { ExitCode::from(2) } else { ExitCode::SUCCESS })
}

/// Raw mode + alternate screen: the editor takes over the whole terminal and
/// receives every keystroke before the shell can interpret it.
fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Terminal::new(CrosstermBackend::new(stdout))
}

/// ALWAYS undo it on the way out — otherwise the terminal is left unusable.
fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()
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
            Event::Key(key) if key.kind == KeyEventKind::Press => app.handle_key(key),
            Event::Resize(_, _) => {}
            _ => {}
        }
    }
    Ok(app.discarded)
}
