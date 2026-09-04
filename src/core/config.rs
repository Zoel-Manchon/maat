//! User configuration, read once at startup.
//!
//! WHY THE PARSER IS HAND-WRITTEN
//!   The obvious move is `toml` + `serde`, and for most projects it is the
//!   right one. Here it would roughly quadruple a dependency tree that is
//!   currently three crates, in a binary whose reason to exist is being small
//!   and auditable enough to ship inside an appliance image. The configuration
//!   is a dozen scalars in two tables; that is worth eighty lines of parser and
//!   not worth ten thousand of someone else's.
//!
//!   So this reads a **deliberate subset of TOML**: comments, bare keys,
//!   `[table]` headers, and string, boolean and integer values. No arrays, no
//!   nested tables, no multi-line strings, no dates. A file using those is not
//!   rejected outright — the unknown lines are collected and reported, so a
//!   config that reaches too far says so instead of silently doing nothing.
//!
//! WHERE IT LIVES
//!   `$MAAT_CONFIG` if set, else `$XDG_CONFIG_HOME/maat/config.toml`, else
//!   `~/.config/maat/config.toml`, and `%APPDATA%\maat\config.toml` on Windows.
//!   A missing file is not an error: the defaults are the shipped behaviour,
//!   and an appliance with a read-only root must boot without one.

use std::fs;
use std::path::{Path, PathBuf};

/// Everything a config file can change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Show line numbers relative to the cursor.
    pub relative_numbers: bool,
    /// Mirror yanks to the terminal clipboard with OSC 52.
    pub clipboard: bool,
    /// Columns a tab stands for.
    pub tab_width: usize,
    /// Insert spaces instead of a tab character.
    pub expand_tabs: bool,
    /// Force the 16-colour palette instead of detecting it.
    pub force_16_colour: bool,
    /// Lines of undo history kept.
    pub history_limit: usize,
    /// Raw `[keys]` bindings as `(pressed, acts_as)`, case preserved.
    ///
    /// Left as strings on purpose: turning them into key codes needs
    /// crossterm, and `core` does not depend on the terminal. `ui::keymap`
    /// does that translation.
    pub keys: Vec<(String, String)>,
    /// Lines that were understood but not recognised, reported once at startup
    /// so a typo in a key name is visible rather than silently ignored.
    pub unknown: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            relative_numbers: false,
            clipboard: false,
            tab_width: 4,
            expand_tabs: false,
            force_16_colour: false,
            // Deep enough that no realistic editing session hits it, small
            // enough that 512 full buffer snapshots stay cheap on an appliance.
            history_limit: 512,
            keys: Vec::new(),
            unknown: Vec::new(),
        }
    }
}

impl Config {
    /// Loads the config from the first path that exists. Never fails: a
    /// missing, unreadable or malformed file leaves the defaults in place,
    /// because an editor that refuses to start over its own config file is
    /// worse than one that ignores it.
    pub fn load() -> Self {
        match Self::path() {
            Some(path) => match fs::read_to_string(&path) {
                Ok(text) => Self::parse(&text),
                Err(_) => Self::default(),
            },
            None => Self::default(),
        }
    }

    /// Where the config file would be, whether or not it exists.
    pub fn path() -> Option<PathBuf> {
        if let Ok(explicit) = std::env::var("MAAT_CONFIG") {
            if !explicit.is_empty() {
                return Some(PathBuf::from(explicit));
            }
        }

        #[cfg(windows)]
        let base = std::env::var("APPDATA").ok().map(PathBuf::from);

        #[cfg(not(windows))]
        let base = std::env::var("XDG_CONFIG_HOME")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var("HOME").ok().map(|home| Path::new(&home).join(".config")));

        base.map(|base| base.join("maat").join("config.toml"))
    }

    /// Parses the supported subset. Unrecognised keys land in `unknown`.
    pub fn parse(text: &str) -> Self {
        let mut config = Self::default();
        let mut table = String::new();

        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }

            if let Some(name) = line.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')) {
                table = name.trim().to_ascii_lowercase();
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                config.unknown.push(line.to_string());
                continue;
            };
            // Everywhere but `[keys]`, option names are words and case does
            // not matter. Inside it, the name *is* a key: `g` and `G` are two
            // different commands, so the case has to survive.
            let raw_key = key.trim().to_string();
            let key = if table == "keys" { raw_key.clone() } else { raw_key.to_ascii_lowercase() };
            let value = value.trim();
            let qualified = if table.is_empty() {
                key.clone()
            } else {
                format!("{table}.{key}")
            };

            match qualified.as_str() {
                "display.relativenumber" | "relativenumber" => {
                    assign_bool(value, &mut config.relative_numbers, line, &mut config.unknown)
                }
                "display.tabwidth" | "tabwidth" => {
                    assign_usize(value, &mut config.tab_width, line, &mut config.unknown)
                }
                "display.expandtabs" | "expandtabs" => {
                    assign_bool(value, &mut config.expand_tabs, line, &mut config.unknown)
                }
                "display.force16colour" | "force16colour" => {
                    assign_bool(value, &mut config.force_16_colour, line, &mut config.unknown)
                }
                "editor.clipboard" | "clipboard" => {
                    assign_bool(value, &mut config.clipboard, line, &mut config.unknown)
                }
                "editor.historylimit" | "historylimit" => {
                    assign_usize(value, &mut config.history_limit, line, &mut config.unknown)
                }
                _ if table == "keys" => {
                    // Values are quoted in TOML; the quotes are not part of
                    // the key name.
                    let target = value.trim_matches('"').to_string();
                    config.keys.push((key.clone(), target));
                }
                _ => config.unknown.push(line.to_string()),
            }
        }

        // A tab of zero columns would make the cursor arithmetic meaningless,
        // and a history of zero would quietly disable undo. Clamp rather than
        // reject: the intent is obvious and the file still loads.
        config.tab_width = config.tab_width.clamp(1, 16);
        config.history_limit = config.history_limit.clamp(1, 100_000);
        config
    }
}

fn strip_comment(line: &str) -> &str {
    // Only outside quotes, so a `#` inside a string value survives.
    let mut in_string = false;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..index],
            _ => {}
        }
    }
    line
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn assign_bool(value: &str, target: &mut bool, line: &str, unknown: &mut Vec<String>) {
    match parse_bool(value) {
        Some(parsed) => *target = parsed,
        None => unknown.push(line.to_string()),
    }
}

fn assign_usize(value: &str, target: &mut usize, line: &str, unknown: &mut Vec<String>) {
    match value.parse::<usize>() {
        Ok(parsed) => *target = parsed,
        Err(_) => unknown.push(line.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_file_leaves_every_default_in_place() {
        assert_eq!(Config::parse(""), Config::default());
    }

    #[test]
    fn reads_tables_and_scalars() {
        let config = Config::parse(
            r#"
            [display]
            relativenumber = true
            tabwidth = 8
            expandtabs = true

            [editor]
            clipboard = true
            historylimit = 100
            "#,
        );

        assert!(config.relative_numbers);
        assert_eq!(config.tab_width, 8);
        assert!(config.expand_tabs);
        assert!(config.clipboard);
        assert_eq!(config.history_limit, 100);
        assert!(config.unknown.is_empty());
    }

    #[test]
    fn keys_work_with_or_without_their_table() {
        let with = Config::parse("[editor]\nclipboard = true");
        let without = Config::parse("clipboard = true");
        assert!(with.clipboard && without.clipboard);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let config = Config::parse(
            "# maat config\n\n  [display]  # the display table\n  tabwidth = 2 # two, like a sane person\n",
        );
        assert_eq!(config.tab_width, 2);
        assert!(config.unknown.is_empty());
    }

    #[test]
    fn a_hash_inside_a_string_is_not_a_comment() {
        // Not used by any current key, but the parser must not mangle it the
        // day one arrives.
        assert_eq!(
            strip_comment(r##"colour = "#ff0000" # red"##).trim(),
            r##"colour = "#ff0000""##
        );
    }

    #[test]
    fn keys_are_case_insensitive() {
        let config = Config::parse("[Display]\nRelativeNumber = true");
        assert!(config.relative_numbers);
    }

    #[test]
    fn an_unknown_key_is_reported_rather_than_swallowed() {
        let config = Config::parse("[display]\nrelatvenumber = true");
        assert!(!config.relative_numbers);
        assert_eq!(config.unknown.len(), 1);
        assert!(config.unknown[0].contains("relatvenumber"));
    }

    #[test]
    fn a_value_of_the_wrong_type_is_reported_and_the_default_kept() {
        let config = Config::parse("tabwidth = wide\nclipboard = yes");
        assert_eq!(config.tab_width, 4, "default kept");
        assert!(!config.clipboard);
        assert_eq!(config.unknown.len(), 2);
    }

    #[test]
    fn nonsense_values_are_clamped_instead_of_breaking_the_editor() {
        // A tab of zero columns makes cursor arithmetic meaningless and a
        // history of zero silently disables undo.
        let config = Config::parse("tabwidth = 0\nhistorylimit = 0");
        assert_eq!(config.tab_width, 1);
        assert_eq!(config.history_limit, 1);

        let config = Config::parse("tabwidth = 999");
        assert_eq!(config.tab_width, 16);
    }

    #[test]
    fn the_keys_table_collects_bindings_in_order() {
        let config = Config::parse("[keys]\nt = \"j\"\nn = \"k\"");
        assert_eq!(
            config.keys,
            vec![("t".into(), "j".into()), ("n".into(), "k".into())]
        );
        assert!(config.unknown.is_empty(), "bindings are not 'unknown' options");
    }

    #[test]
    fn key_names_keep_their_case_while_option_names_do_not() {
        // `g` and `G` are different commands, so a binding's case is meaning.
        let config = Config::parse("[keys]\nG = \"j\"\n\n[display]\nRelativeNumber = true");
        assert_eq!(config.keys, vec![("G".into(), "j".into())]);
        assert!(config.relative_numbers, "option names stay case-insensitive");
    }

    #[test]
    fn a_binding_is_read_without_its_quotes() {
        assert_eq!(Config::parse("[keys]\nt = \"Left\"").keys, vec![("t".into(), "Left".into())]);
        assert_eq!(Config::parse("[keys]\nt = Left").keys, vec![("t".into(), "Left".into())]);
    }

    #[test]
    fn a_line_that_is_not_a_key_value_pair_is_reported() {
        let config = Config::parse("this is not toml");
        assert_eq!(config.unknown.len(), 1);
    }

    #[test]
    fn the_explicit_path_wins_over_every_convention() {
        // Serial, because it touches process-wide state.
        unsafe { std::env::set_var("MAAT_CONFIG", "/tmp/maat-explicit.toml") };
        assert_eq!(Config::path(), Some(PathBuf::from("/tmp/maat-explicit.toml")));
        unsafe { std::env::remove_var("MAAT_CONFIG") };
    }
}
