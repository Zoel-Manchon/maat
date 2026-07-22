//! Editor modes.
//!
//! Vim's central idea: the same key means different things depending on the
//! mode. Modelling it as an enum makes the compiler force every input state
//! to be handled explicitly.

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Insert,
    Command,
    Search,
}

impl Mode {
    /// Label for the status bar.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Command => "COMMAND",
            Mode::Search => "SEARCH",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_normal() {
        assert_eq!(Mode::default(), Mode::Normal);
    }

    #[test]
    fn every_mode_has_a_label() {
        for mode in [Mode::Normal, Mode::Insert, Mode::Command, Mode::Search] {
            assert!(!mode.label().is_empty());
        }
    }
}
