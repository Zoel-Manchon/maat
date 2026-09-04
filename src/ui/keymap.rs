//! Remapping keys, without a second copy of the command table.
//!
//! THE APPROACH
//!   A keymap could be modelled as "key → action", which means an `Action`
//!   enum, a lookup table, and a dispatch path that has to be kept in step
//!   with the built-in one forever. Every command added later has to be
//!   remembered in two places, and the day someone forgets, a rebound key
//!   silently does nothing.
//!
//!   This does the other thing: it rewrites the incoming key into the key the
//!   editor already understands. "When I press `t`, act as if I pressed `j`."
//!   That is what a user means by remapping anyway, it is about thirty lines,
//!   and it composes with everything for free — counts, operator-pending,
//!   visual mode and the two-stroke sequences all keep working, because by the
//!   time they see the key it is already the canonical one.
//!
//!   The cost is honest and worth stating: you can only bind a key to another
//!   *key*, not to an arbitrary command. `t = "j"` works; `t = "delete_line"`
//!   does not — you write `t = "d"` and press it twice, exactly as you would
//!   have pressed `dd`.
//!
//! WHY ANYONE WANTS THIS
//!   `hjkl` is a home row on QWERTY and scattered nonsense on Dvorak, Colemak
//!   or an AZERTY laptop. On a Spanish keyboard `$` needs a modifier. The
//!   appliance console this editor targets is whatever keyboard is plugged
//!   into it.
//!
//! ONE PASS, NEVER TWO
//!   With `t = "j"` and `j = "k"`, pressing `t` gives `j` — not `k`. The map
//!   is applied exactly once, so a set of bindings cannot chain into something
//!   nobody wrote, and swapping a pair (`j = "k"`, `k = "j"`) does what it
//!   looks like instead of collapsing.

use crossterm::event::KeyCode;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyMap {
    entries: Vec<(KeyCode, KeyCode)>,
}

impl KeyMap {
    /// Builds a map from `(pressed, acts_as)` config pairs.
    ///
    /// Returns the map plus every line it could not make sense of, so the
    /// caller can say so out loud rather than leaving a binding that silently
    /// does nothing.
    pub fn from_pairs(pairs: &[(String, String)]) -> (Self, Vec<String>) {
        let mut entries = Vec::new();
        let mut rejected = Vec::new();

        for (pressed, acts_as) in pairs {
            match (parse_key(pressed), parse_key(acts_as)) {
                (Some(from), Some(to)) if from == to => {
                    // Harmless, but almost certainly a mistake worth surfacing.
                    rejected.push(format!("{pressed} = \"{acts_as}\" (binds a key to itself)"));
                }
                (Some(from), Some(to)) => entries.push((from, to)),
                _ => rejected.push(format!("{pressed} = \"{acts_as}\"")),
            }
        }

        (Self { entries }, rejected)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The key the editor should act on. Applied once; the result is never
    /// looked up again.
    pub fn resolve(&self, key: KeyCode) -> KeyCode {
        self.entries
            .iter()
            .find(|(from, _)| *from == key)
            .map(|(_, to)| *to)
            .unwrap_or(key)
    }
}

/// Parses a key spec: a single character, or one of the named keys.
///
/// Named keys are matched case-insensitively (`Esc`, `esc`, `ESC`) because a
/// name is a word; single characters are **not**, because `g` and `G` are two
/// different commands and always have been.
pub fn parse_key(spec: &str) -> Option<KeyCode> {
    let mut chars = spec.chars();
    if let (Some(only), None) = (chars.next(), chars.next()) {
        return Some(KeyCode::Char(only));
    }

    match spec.to_ascii_lowercase().as_str() {
        "space" => Some(KeyCode::Char(' ')),
        "enter" | "return" | "cr" => Some(KeyCode::Enter),
        "esc" | "escape" => Some(KeyCode::Esc),
        "tab" => Some(KeyCode::Tab),
        "backspace" | "bs" => Some(KeyCode::Backspace),
        "left" => Some(KeyCode::Left),
        "right" => Some(KeyCode::Right),
        "up" => Some(KeyCode::Up),
        "down" => Some(KeyCode::Down),
        "home" => Some(KeyCode::Home),
        "end" => Some(KeyCode::End),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> KeyMap {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(a, b)| ((*a).to_string(), (*b).to_string()))
            .collect();
        KeyMap::from_pairs(&owned).0
    }

    #[test]
    fn an_empty_map_leaves_every_key_alone() {
        let map = KeyMap::default();
        assert!(map.is_empty());
        assert_eq!(map.resolve(KeyCode::Char('j')), KeyCode::Char('j'));
    }

    #[test]
    fn a_bound_key_resolves_to_its_target() {
        // The Dvorak case: down is under the index finger again.
        let map = map(&[("t", "j"), ("n", "k")]);
        assert_eq!(map.resolve(KeyCode::Char('t')), KeyCode::Char('j'));
        assert_eq!(map.resolve(KeyCode::Char('n')), KeyCode::Char('k'));
    }

    #[test]
    fn an_unbound_key_passes_through_untouched() {
        let map = map(&[("t", "j")]);
        assert_eq!(map.resolve(KeyCode::Char('x')), KeyCode::Char('x'));
        assert_eq!(map.resolve(KeyCode::Esc), KeyCode::Esc);
    }

    #[test]
    fn the_map_is_applied_once_and_never_chains() {
        // t → j and j → k. Pressing `t` must give `j`, not `k`.
        let map = map(&[("t", "j"), ("j", "k")]);
        assert_eq!(map.resolve(KeyCode::Char('t')), KeyCode::Char('j'));
    }

    #[test]
    fn a_pair_can_be_swapped() {
        // Which only works because resolution does not chain.
        let map = map(&[("j", "k"), ("k", "j")]);
        assert_eq!(map.resolve(KeyCode::Char('j')), KeyCode::Char('k'));
        assert_eq!(map.resolve(KeyCode::Char('k')), KeyCode::Char('j'));
    }

    #[test]
    fn case_matters_for_characters() {
        // `g` and `G` are different commands; a map that conflated them would
        // make `gg` and `G` the same key.
        let map = map(&[("G", "j")]);
        assert_eq!(map.resolve(KeyCode::Char('G')), KeyCode::Char('j'));
        assert_eq!(map.resolve(KeyCode::Char('g')), KeyCode::Char('g'));
    }

    #[test]
    fn named_keys_parse_in_any_case() {
        for spec in ["Esc", "esc", "ESCAPE"] {
            assert_eq!(parse_key(spec), Some(KeyCode::Esc), "{spec}");
        }
        assert_eq!(parse_key("Space"), Some(KeyCode::Char(' ')));
        assert_eq!(parse_key("Left"), Some(KeyCode::Left));
        assert_eq!(parse_key("Enter"), Some(KeyCode::Enter));
    }

    #[test]
    fn an_arrow_key_can_be_bound_like_any_other() {
        let map = map(&[("Left", "h")]);
        assert_eq!(map.resolve(KeyCode::Left), KeyCode::Char('h'));
    }

    #[test]
    fn nonsense_is_rejected_and_reported_rather_than_ignored() {
        let owned = vec![
            ("t".to_string(), "j".to_string()),
            ("qq".to_string(), "j".to_string()),
            ("t".to_string(), "nowhere".to_string()),
        ];
        let (map, rejected) = KeyMap::from_pairs(&owned);
        assert_eq!(rejected.len(), 2);
        assert_eq!(map.resolve(KeyCode::Char('t')), KeyCode::Char('j'));
    }

    #[test]
    fn binding_a_key_to_itself_is_reported() {
        let owned = vec![("j".to_string(), "j".to_string())];
        let (map, rejected) = KeyMap::from_pairs(&owned);
        assert!(map.is_empty());
        assert_eq!(rejected.len(), 1);
        assert!(rejected[0].contains("itself"));
    }

    #[test]
    fn a_multibyte_character_is_a_single_key() {
        // A key spec is one *character*, not one byte: 'ñ' must bind.
        let map = map(&[("ñ", "j")]);
        assert_eq!(map.resolve(KeyCode::Char('ñ')), KeyCode::Char('j'));
    }
}
