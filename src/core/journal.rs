//! Crash recovery: a journal of unsaved work.
//!
//! Atomic saves already guarantee that a crash cannot leave a *half-written
//! file*. They say nothing about the other half of the problem: the twenty
//! minutes of editing that only ever existed in memory. On a server that lost
//! power, or a session someone killed, or a panic on an appliance console with
//! no second terminal to recover from, that work is simply gone.
//!
//! So every so often the buffer is written to a journal under the editor's
//! state directory, and the journal is deleted the moment the work is safely
//! on disk. A journal that outlives the session is therefore a statement: this
//! buffer had unsaved changes when the editor stopped.
//!
//! WHAT IT IS NOT
//!   Not a lock file. maat does not stop a second editor from opening the same
//!   path, because a stale lock on an appliance is worse than the race it
//!   prevents — it turns a recoverable situation into one that needs someone
//!   who knows to go and delete a file.
//!
//! WHY IT RECORDS THE DISK HASH
//!   Recovery is only safe if the file underneath is the one the journal was
//!   taken against. If it changed in the meantime, restoring the journal would
//!   silently throw away whatever the other party wrote — exactly the blind
//!   overwrite the rest of this editor exists to prevent. The hash makes that
//!   case visible instead of quiet.
//!
//! WHY IT IS A STRUCT AND NOT FREE FUNCTIONS
//!   The directory is resolved once, at construction, and carried. Reading the
//!   environment inside every call would make the whole module depend on
//!   process-wide state — untestable in parallel, and surprising the day
//!   something changes a variable mid-run.

use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAGIC: &str = "maat-journal 1";
const SEPARATOR: &str = "--";

/// What was found in a journal left behind by a previous session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recovered {
    /// The buffer as it stood when the journal was last written.
    pub text: String,
    /// The file's hash when that session opened it, or `None` for a buffer
    /// that had never been saved.
    pub opened_hash: Option<String>,
    /// Unix seconds.
    pub saved_at: u64,
}

/// A directory that holds journals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Journal {
    dir: PathBuf,
}

impl Journal {
    /// Resolves the state directory: `$MAAT_STATE`, else
    /// `$XDG_STATE_HOME/maat`, else `~/.local/state/maat`, and
    /// `%LOCALAPPDATA%\maat` on Windows.
    ///
    /// `None` when there is nowhere to write — a read-only root with no
    /// `HOME`. The editor then works exactly as it did before this module
    /// existed; it simply cannot offer recovery.
    pub fn discover() -> Option<Self> {
        if let Ok(explicit) = std::env::var("MAAT_STATE") {
            if !explicit.is_empty() {
                return Some(Self::in_dir(PathBuf::from(explicit)));
            }
        }

        #[cfg(windows)]
        let base = std::env::var("LOCALAPPDATA").ok().map(PathBuf::from);

        #[cfg(not(windows))]
        let base = std::env::var("XDG_STATE_HOME")
            .ok()
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .filter(|value| !value.is_empty())
                    .map(|home| Path::new(&home).join(".local").join("state"))
            });

        base.map(|base| Self::in_dir(base.join("maat")))
    }

    pub fn in_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The journal file for a document.
    ///
    /// Named by the hash of the absolute path rather than by the path itself:
    /// a flat directory, no nesting to create, no length limits to trip over,
    /// and two files with the same basename in different directories never
    /// collide. The original path is stored inside the journal, so nothing is
    /// lost by not having it in the name.
    pub fn path_for(&self, document: &Path) -> PathBuf {
        let absolute = fs::canonicalize(document).unwrap_or_else(|_| document.to_path_buf());
        let key = short_hash(&absolute.to_string_lossy());
        self.dir.join("swap").join(format!("{key}.maat-journal"))
    }

    /// Writes the journal. The caller may ignore the error: a full disk or an
    /// unwritable state directory must never cost the user their edit.
    pub fn write(&self, document: &Path, text: &str, opened_hash: Option<&str>) -> io::Result<()> {
        let path = self.path_for(document);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let header = format!(
            "{MAGIC}\npath {}\nopened-hash {}\nsaved-at {}\n{SEPARATOR}\n",
            document.display(),
            opened_hash.unwrap_or("-"),
            now_secs()
        );

        fs::write(&path, format!("{header}{text}"))
    }

    /// Reads a journal, if one is there and looks like ours.
    pub fn read(&self, document: &Path) -> Option<Recovered> {
        let raw = fs::read_to_string(self.path_for(document)).ok()?;

        // Only the first separator counts, so a document that itself contains
        // a line of two dashes is not truncated at it.
        let (header, text) = raw.split_once(&format!("\n{SEPARATOR}\n"))?;
        let mut lines = header.lines();
        if lines.next()? != MAGIC {
            return None;
        }

        let mut opened_hash = None;
        let mut saved_at = 0;
        for line in lines {
            match line.split_once(' ') {
                Some(("opened-hash", value)) if value != "-" => {
                    opened_hash = Some(value.to_string())
                }
                Some(("saved-at", value)) => saved_at = value.parse().unwrap_or(0),
                _ => {}
            }
        }

        Some(Recovered {
            text: text.to_string(),
            opened_hash,
            saved_at,
        })
    }

    /// Deletes the journal. Called after a successful save and on a clean
    /// quit: past that point it describes work that is already on disk, and
    /// leaving it behind would raise a false alarm on the next open.
    pub fn remove(&self, document: &Path) {
        let _ = fs::remove_file(self.path_for(document));
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// First 16 hex characters of the SHA-256 — 64 bits, far more than enough to
/// keep one person's open files apart.
fn short_hash(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize()).chars().take(16).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A journal in its own scratch directory. No environment variables, so
    /// these tests run in parallel without stepping on each other.
    fn journal(name: &str) -> Journal {
        let dir = std::env::temp_dir().join(format!("maat_journal_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Journal::in_dir(dir)
    }

    #[test]
    fn a_journal_round_trips() {
        let journal = journal("roundtrip");
        let document = Path::new("/tmp/notes.txt");
        journal.write(document, "uno\ndos", Some("abc123")).unwrap();

        let recovered = journal.read(document).expect("a journal");
        assert_eq!(recovered.text, "uno\ndos");
        assert_eq!(recovered.opened_hash.as_deref(), Some("abc123"));
        assert!(recovered.saved_at > 0);
    }

    #[test]
    fn there_is_no_journal_until_one_is_written() {
        assert_eq!(journal("absent").read(Path::new("/tmp/never-touched.txt")), None);
    }

    #[test]
    fn removing_a_journal_makes_it_stop_reporting() {
        let journal = journal("remove");
        let document = Path::new("/tmp/notes.txt");
        journal.write(document, "algo", None).unwrap();
        assert!(journal.read(document).is_some());

        journal.remove(document);
        assert_eq!(journal.read(document), None);
    }

    #[test]
    fn two_files_with_the_same_name_get_different_journals() {
        let journal = journal("collision");
        let a = Path::new("/tmp/one/config.toml");
        let b = Path::new("/tmp/two/config.toml");
        assert_ne!(journal.path_for(a), journal.path_for(b));

        journal.write(a, "de uno", None).unwrap();
        journal.write(b, "de dos", None).unwrap();
        assert_eq!(journal.read(a).unwrap().text, "de uno");
        assert_eq!(journal.read(b).unwrap().text, "de dos");
    }

    #[test]
    fn a_buffer_that_was_never_saved_has_no_opened_hash() {
        let journal = journal("nohash");
        let document = Path::new("/tmp/fresh.txt");
        journal.write(document, "nuevo", None).unwrap();
        assert_eq!(journal.read(document).unwrap().opened_hash, None);
    }

    #[test]
    fn text_containing_the_separator_survives() {
        let journal = journal("separator");
        let document = Path::new("/tmp/dashes.txt");
        let text = "antes\n--\ndespues";
        journal.write(document, text, None).unwrap();
        assert_eq!(journal.read(document).unwrap().text, text);
    }

    #[test]
    fn a_file_that_is_not_one_of_ours_is_ignored() {
        let journal = journal("foreign");
        let document = Path::new("/tmp/notes.txt");
        let path = journal.path_for(document);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "algo que no escribimos nosotros\n--\ncuerpo").unwrap();

        assert_eq!(journal.read(document), None, "no magic line, not our journal");
    }

    #[test]
    fn an_unwritable_directory_is_an_error_and_not_a_panic() {
        // The read-only-root case. `write` reports it; every caller ignores it.
        let journal = Journal::in_dir(PathBuf::from("/proc/definitely-not-writable"));
        assert!(journal.write(Path::new("/tmp/x.txt"), "algo", None).is_err());
        assert_eq!(journal.read(Path::new("/tmp/x.txt")), None);
    }
}
