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
    /// Character-wise selection (`v`): the span runs from the anchor to the
    /// cursor, both ends included.
    Visual,
    /// Line-wise selection (`V`): whole lines, regardless of the columns.
    VisualLine,
}

impl Mode {
    /// Label for the status bar.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Command => "COMMAND",
            Mode::Search => "SEARCH",
            Mode::Visual => "VISUAL",
            Mode::VisualLine => "V-LINE",
        }
    }

    /// Is a selection being made? Both visual modes answer yes, and the
    /// cursor treats them like Normal: it sits *on* a character, not past the
    /// last one.
    pub fn is_visual(self) -> bool {
        matches!(self, Mode::Visual | Mode::VisualLine)
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
        for mode in [
            Mode::Normal,
            Mode::Insert,
            Mode::Command,
            Mode::Search,
            Mode::Visual,
            Mode::VisualLine,
        ] {
            assert!(!mode.label().is_empty());
        }
    }

    #[test]
    fn only_the_two_visual_modes_are_visual() {
        assert!(Mode::Visual.is_visual());
        assert!(Mode::VisualLine.is_visual());
        assert!(!Mode::Normal.is_visual());
        assert!(!Mode::Insert.is_visual());
    }
}
