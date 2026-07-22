//! Structured audit events.
//!
//! On a hardened appliance, editing a config file is a security-relevant act.
//! Every save can emit one machine-readable line recording *what* changed and
//! *to what*, so a SOC pipeline gets a tamper-evident trail of configuration
//! changes rather than a silent mutation.
//!
//! Off by default. Set `MAAT_AUDIT_LOG` to a path to enable it, and
//! `MAAT_AUDIT_FORMAT=cef` to emit CEF instead of JSON — the same two
//! formats `phosphor` exports, so both tools feed one collector.
//!
//! Nothing here is allowed to break a save: audit failures are silent by
//! design. Losing the user's edit because a log file was unwritable would be
//! a far worse outcome than losing one audit line.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single "the operator wrote this file" event.
pub struct SaveEvent<'a> {
    pub path: &'a Path,
    /// Hash of what was on disk before this write, when known.
    pub hash_before: Option<&'a str>,
    /// Hash of what is on disk now.
    pub hash_after: &'a str,
    pub lines: usize,
}

impl SaveEvent<'_> {
    /// Seconds since the Unix epoch. Deliberately not a formatted date: no
    /// extra dependency, and collectors parse epochs happily.
    fn timestamp() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|delta| delta.as_secs())
            .unwrap_or(0)
    }

    /// Best-effort operator identity, without pulling in a libc binding.
    fn operator() -> String {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "unknown".into())
    }

    pub fn to_json(&self) -> String {
        format!(
            r#"{{"tool":"maat","event":"save","ts":{},"user":"{}","path":"{}","lines":{},"sha256_before":{},"sha256_after":"{}"}}"#,
            Self::timestamp(),
            escape(&Self::operator()),
            escape(&self.path.display().to_string()),
            self.lines,
            match self.hash_before {
                Some(hash) => format!("\"{hash}\""),
                None => "null".into(),
            },
            self.hash_after,
        )
    }

    /// Common Event Format, as consumed by ArcSight and most SIEMs.
    pub fn to_cef(&self) -> String {
        format!(
            "CEF:0|maat|maat|0.3|File_Written|File Written|3|rt={} suser={} fname={} cnt={} oldFileHash={} fileHash={}",
            Self::timestamp() * 1000,
            Self::operator(),
            self.path.display(),
            self.lines,
            self.hash_before.unwrap_or("-"),
            self.hash_after,
        )
    }
}

/// Escapes the few characters that would otherwise break the hand-rolled JSON.
/// Writing a serialiser by hand is only defensible because the shape is fixed
/// and tiny; anything richer would justify pulling in `serde_json`.
fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Appends the event to `$MAAT_AUDIT_LOG`, if that variable is set.
/// Returns `Ok(false)` when auditing is simply switched off.
pub fn log(event: &SaveEvent) -> io::Result<bool> {
    let Ok(target) = std::env::var("MAAT_AUDIT_LOG") else {
        return Ok(false);
    };
    if target.is_empty() {
        return Ok(false);
    }

    let cef = std::env::var("MAAT_AUDIT_FORMAT")
        .map(|format| format.eq_ignore_ascii_case("cef"))
        .unwrap_or(false);
    let line = if cef { event.to_cef() } else { event.to_json() };

    let mut file = OpenOptions::new().create(true).append(true).open(target)?;
    writeln!(file, "{line}")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> SaveEvent<'static> {
        SaveEvent {
            path: Path::new("/etc/emberwall/gateway.conf"),
            hash_before: Some("aaa111"),
            hash_after: "bbb222",
            lines: 42,
        }
    }

    #[test]
    fn json_carries_both_hashes() {
        let json = sample().to_json();
        assert!(json.contains(r#""sha256_before":"aaa111""#));
        assert!(json.contains(r#""sha256_after":"bbb222""#));
        assert!(json.contains(r#""event":"save""#));
    }

    #[test]
    fn json_uses_null_for_a_brand_new_file() {
        let event = SaveEvent { hash_before: None, ..sample() };
        assert!(event.to_json().contains(r#""sha256_before":null"#));
    }

    #[test]
    fn cef_is_one_line_and_carries_the_filename() {
        let cef = sample().to_cef();
        assert_eq!(cef.lines().count(), 1);
        assert!(cef.starts_with("CEF:0|maat"));
        assert!(cef.contains("fname=/etc/emberwall/gateway.conf"));
    }

    #[test]
    fn escaping_keeps_the_json_parseable() {
        assert_eq!(escape(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape("line\nbreak"), "line\\nbreak");
    }

    #[test]
    fn logging_is_a_no_op_when_disabled() {
        // Safe here: no other test in this module touches the variable.
        std::env::remove_var("MAAT_AUDIT_LOG");
        assert!(!log(&sample()).unwrap());
    }
}
