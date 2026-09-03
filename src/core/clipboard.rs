//! Copying to the system clipboard through the terminal itself (OSC 52).
//!
//! WHY NOT A CLIPBOARD CRATE
//!   The obvious answer is `arboard` or `copypasta`, and both are the wrong
//!   dependency here. They talk to X11, Wayland or the Win32 API — none of
//!   which exist on the appliance console this editor is built for, where the
//!   session is a serial line or an SSH connection and there is no display
//!   server at all. On that box a clipboard crate links a pile of graphics
//!   libraries and then fails at runtime.
//!
//!   OSC 52 inverts the problem: instead of reaching for the clipboard of the
//!   machine the editor runs on, it asks the *terminal emulator* — the one on
//!   the operator's laptop, at the far end of the SSH connection — to put the
//!   text on its own clipboard. That is almost always the clipboard the person
//!   actually wants, and it costs one escape sequence and no dependencies.
//!
//! THE OTHER DIRECTION
//!   Reading the clipboard back over OSC 52 requires the terminal to answer,
//!   which most of them refuse to do — a page that can read your clipboard is
//!   a genuine hazard, so terminals disable it. Pasting in is already covered
//!   by bracketed paste, which is the path a person uses anyway: Ctrl-V into
//!   the terminal, and the block arrives as one event.
//!
//! COMPATIBILITY
//!   Supported by xterm, kitty, alacritty, wezterm, foot, iTerm2 and Windows
//!   Terminal; tmux and screen need it enabled (`set -g set-clipboard on`). A
//!   terminal that does not understand the sequence ignores it, but a few older
//!   ones echo it as text, which is why this is opt-in rather than always on.

/// Wraps `text` in the OSC 52 sequence that sets the terminal's clipboard.
///
/// `ESC ] 52 ; c ; <base64> BEL` — `c` is the clipboard selection, and BEL is
/// used as the terminator because more terminals accept it than the formally
/// correct `ESC \`.
pub fn osc52_copy(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()))
}

/// Standard base64, written out rather than pulled in.
///
/// A dependency for forty lines of table lookup is not a trade worth making in
/// a project whose selling point is a small, auditable binary on an appliance.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);

    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_the_rfc_4648_test_vectors() {
        // Verifiable with: printf '%s' "foobar" | base64
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn encodes_multibyte_text_by_its_utf8_bytes() {
        assert_eq!(base64_encode("café".as_bytes()), "Y2Fmw6k=");
    }

    #[test]
    fn wraps_the_payload_in_the_osc_52_sequence() {
        let sequence = osc52_copy("foobar");
        assert!(sequence.starts_with("\x1b]52;c;"));
        assert!(sequence.ends_with('\x07'));
        assert!(sequence.contains("Zm9vYmFy"));
    }

    #[test]
    fn a_multiline_yank_survives_the_round_trip_to_base64() {
        // Newlines are payload, not terminators: the sequence must carry them
        // through rather than ending early.
        let sequence = osc52_copy("uno\ndos");
        assert_eq!(sequence.matches('\x07').count(), 1);
        assert!(!sequence.contains('\n'));
    }
}
