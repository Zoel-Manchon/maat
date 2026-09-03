//! The document: buffer + file on disk + **integrity awareness**.
//!
//! This is the piece that makes maat more than another vim clone. Opening a
//! file records its SHA-256; before writing we can hash it again and detect
//! whether *another process* changed it while we were editing.
//!
//! The case it guards against isn't theoretical: on an unattended system (an
//! Emberwall appliance, a shared server) a blind save silently destroys
//! someone else's change — or an attacker's tracks. An editor that knows how
//! to hash can warn instead of destroying evidence.

use sha2::{Digest, Sha256};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use super::buffer::Buffer;

/// How the file terminates its lines.
///
/// The buffer always holds lines without terminators, so everything above this
/// module works in one representation. What the file used is remembered here
/// and put back on save: opening a CRLF file on Linux and saving it must not
/// silently rewrite every line, which would turn a one-word edit into a diff
/// against the whole file — and, on a config an appliance ships, into a
/// checksum mismatch nobody asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    /// A bare newline — the default for a new buffer, and for anything mixed.
    Lf,
    /// Carriage return + newline, kept byte-for-byte when that is what came in.
    Crlf,
}

impl LineEnding {
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Lf => "\n",
            LineEnding::Crlf => "\r\n",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            LineEnding::Lf => "LF",
            LineEnding::Crlf => "CRLF",
        }
    }

    /// Decide from the text as read.
    ///
    /// A file counts as CRLF when it has at least one CRLF and *every* newline
    /// in it is one. Mixed endings stay LF: rewriting the odd ones out would be
    /// an edit the user never asked for, so those stray carriage returns are
    /// left exactly as they came in, as characters inside the line.
    fn detect(text: &str) -> Self {
        let crlf = text.matches("\r\n").count();
        if crlf > 0 && crlf == text.matches('\n').count() {
            LineEnding::Crlf
        } else {
            LineEnding::Lf
        }
    }
}

#[derive(Debug)]
pub struct Document {
    pub buffer: Buffer,
    path: Option<PathBuf>,
    /// The terminator this file used when we read it, restored on every save.
    line_ending: LineEnding,
    /// SHA-256 of the contents as they were on disk the last time we read or
    /// wrote them. `None` for a buffer with no file behind it yet.
    disk_hash: Option<String>,
    /// Hash of the contents as of the last open/save, so we can tell whether
    /// the user has touched anything since.
    saved_hash: Option<String>,
}

/// What we find when checking the file just before saving.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskState {
    /// The file on disk is still the one we read.
    Unchanged,
    /// Something outside this editor changed it since we opened it.
    ModifiedExternally,
    /// It existed and has since vanished.
    Missing,
    /// Buffer with no file attached yet.
    NoFile,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            buffer: Buffer::default(),
            path: None,
            line_ending: LineEnding::Lf,
            disk_hash: None,
            saved_hash: Some(hash_text("")),
        }
    }
}

impl Document {
    /// Opens a file. If it doesn't exist, prepares a fresh document at that
    /// path (like `vim some-file-that-does-not-exist`).
    pub fn open(path: &Path) -> io::Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let hash = hash_text(&text);
                let line_ending = LineEnding::detect(&text);
                // The buffer never sees the terminators: a CRLF file would
                // otherwise leave a stray carriage return at the end of every
                // line, which renders as a control glyph and throws off every
                // column calculation downstream.
                let normalized = match line_ending {
                    LineEnding::Crlf => text.replace("\r\n", "\n"),
                    LineEnding::Lf => text,
                };
                Ok(Self {
                    buffer: Buffer::from_text(&normalized),
                    path: Some(path.to_path_buf()),
                    line_ending,
                    disk_hash: Some(hash.clone()),
                    saved_hash: Some(hash),
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self {
                path: Some(path.to_path_buf()),
                disk_hash: None,
                ..Default::default()
            }),
            Err(error) => Err(error),
        }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// SHA-256 of the file as it was last read from or written to disk.
    pub fn disk_hash(&self) -> Option<&str> {
        self.disk_hash.as_deref()
    }

    /// Short name for the status bar.
    pub fn name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "[no name]".into())
    }

    /// Are there unsaved changes? Exposed for tests; the app uses the cached
    /// hash instead, to avoid walking the buffer on every render.
    #[cfg(test)]
    pub fn is_modified(&self) -> bool {
        self.is_hash_modified(&self.buffer_hash())
    }

    #[cfg(test)]
    pub fn from_text_for_test(text: &str) -> Self {
        let hash = hash_text(text);
        Self {
            buffer: Buffer::from_text(text),
            path: None,
            line_ending: LineEnding::Lf,
            disk_hash: None,
            saved_hash: Some(hash),
        }
    }

    /// Cheap variant for when the app layer already keeps the buffer hash
    /// cached. Avoids re-hashing the whole document every frame.
    pub fn is_hash_modified(&self, current_hash: &str) -> bool {
        self.saved_hash.as_deref() != Some(current_hash)
    }

    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// The bytes a save would write: the buffer re-joined with the file's own
    /// terminator. Everything that hashes or compares against disk goes
    /// through here, so a CRLF file never looks modified just for being CRLF.
    pub fn to_disk_text(&self) -> String {
        match self.line_ending {
            // The common path, and a hot one: `buffer_hash` runs after every
            // edit, so LF files skip the re-join entirely.
            LineEnding::Lf => self.buffer.to_text(),
            LineEnding::Crlf => {
                let lines: Vec<&str> = self.buffer.iter_lines().collect();
                lines.join(self.line_ending.as_str())
            }
        }
    }

    /// SHA-256 of the **current buffer contents** (what a save would write).
    pub fn buffer_hash(&self) -> String {
        hash_text(&self.to_disk_text())
    }

    /// Compares the file on disk with the one we read. Called before saving:
    /// if it returns `ModifiedExternally`, the UI must ask for confirmation
    /// rather than overwrite.
    pub fn disk_state(&self) -> DiskState {
        let Some(path) = &self.path else { return DiskState::NoFile };
        let Some(known) = &self.disk_hash else {
            // It never existed on disk; if it does now, someone created it.
            return if path.exists() { DiskState::ModifiedExternally } else { DiskState::NoFile };
        };

        match fs::read_to_string(path) {
            Ok(text) if &hash_text(&text) == known => DiskState::Unchanged,
            Ok(_) => DiskState::ModifiedExternally,
            Err(_) => DiskState::Missing,
        }
    }

    /// Writes the buffer to disk **atomically** and re-anchors both hashes.
    pub fn save(&mut self) -> io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "document has no path"));
        };
        let text = self.to_disk_text();
        write_atomic(&path, &text)?;

        let hash = hash_text(&text);
        self.disk_hash = Some(hash.clone());
        self.saved_hash = Some(hash);
        Ok(())
    }

    pub fn save_as(&mut self, path: &Path) -> io::Result<()> {
        let previous_path = self.path.replace(path.to_path_buf());
        let previous_disk_hash = self.disk_hash.take();

        if let Err(error) = self.save() {
            self.path = previous_path;
            self.disk_hash = previous_disk_hash;
            return Err(error);
        }

        Ok(())
    }
}

/// Writes `text` to `path` without ever leaving a half-written file behind.
///
/// A plain `fs::write` truncates the target and then streams into it: if the
/// process is killed or the machine loses power midway, the file on disk is
/// neither the old contents nor the new ones. For an editor whose whole pitch
/// is integrity, that failure mode is unacceptable.
///
/// The sequence is the standard one:
///   1. write to a temporary file **in the same directory** (so the final
///      rename stays within one filesystem, where it is atomic);
///   2. `flush` + `sync_all` so the bytes are really on the device, not just
///      in the kernel's page cache;
///   3. carry over the original permissions;
///   4. `rename` over the target — atomic on POSIX and on Windows;
///   5. sync the directory so the rename itself survives a crash.
fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    let directory = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = directory.join(format!(".{name}.maat-{}.tmp", std::process::id()));

    let write_result = (|| -> io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
        file.sync_all()
    })();

    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    // Preserve the original mode when replacing an existing file.
    if let Ok(metadata) = fs::metadata(path) {
        let _ = fs::set_permissions(&temporary, metadata.permissions());
    }

    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    // Durability of the rename itself. Best-effort: not portable, never fatal.
    #[cfg(unix)]
    {
        if let Ok(handle) = fs::File::open(directory) {
            let _ = handle.sync_all();
        }
    }

    Ok(())
}

fn hash_text(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("maat_test_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn hashes_known_content() {
        // Verifiable with: echo -n "hello world" | sha256sum
        assert_eq!(
            hash_text("hello world"),
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn opens_existing_file_and_starts_unmodified() {
        let dir = temp_dir("open");
        let file = dir.join("a.txt");
        fs::write(&file, "uno\ndos").unwrap();

        let document = Document::open(&file).unwrap();
        assert_eq!(document.buffer.line_count(), 2);
        assert!(!document.is_modified());
        assert_eq!(document.disk_state(), DiskState::Unchanged);
    }

    #[test]
    fn opening_a_missing_file_prepares_a_new_one() {
        let dir = temp_dir("missing");
        let document = Document::open(&dir.join("nuevo.txt")).unwrap();

        assert_eq!(document.buffer.line_count(), 1);
        assert!(!document.is_modified());
    }

    #[test]
    fn tracks_unsaved_changes() {
        let dir = temp_dir("dirty");
        let file = dir.join("b.txt");
        fs::write(&file, "hola").unwrap();

        let mut document = Document::open(&file).unwrap();
        document.buffer.insert_char(0, 4, '!');
        assert!(document.is_modified());

        document.save().unwrap();
        assert!(!document.is_modified());
        assert_eq!(fs::read_to_string(&file).unwrap(), "hola!");
    }

    #[test]
    fn detects_external_modification() {
        let dir = temp_dir("external");
        let file = dir.join("c.txt");
        fs::write(&file, "original").unwrap();

        let document = Document::open(&file).unwrap();
        assert_eq!(document.disk_state(), DiskState::Unchanged);

        // Another process (or an attacker) touches the file underneath us.
        fs::write(&file, "manipulado").unwrap();
        assert_eq!(document.disk_state(), DiskState::ModifiedExternally);
    }

    #[test]
    fn detects_a_vanished_file() {
        let dir = temp_dir("gone");
        let file = dir.join("d.txt");
        fs::write(&file, "still here").unwrap();

        let document = Document::open(&file).unwrap();
        fs::remove_file(&file).unwrap();
        assert_eq!(document.disk_state(), DiskState::Missing);
    }

    #[test]
    fn failed_save_as_restores_the_original_path() {
        let dir = temp_dir("save_as_rollback");
        let original = dir.join("original.txt");
        fs::write(&original, "contenido").unwrap();

        let mut document = Document::open(&original).unwrap();
        let invalid_destination = dir.join("missing-parent").join("copy.txt");

        assert!(document.save_as(&invalid_destination).is_err());
        assert_eq!(document.path(), Some(original.as_path()));
        assert_eq!(document.disk_state(), DiskState::Unchanged);
    }

    #[test]
    fn atomic_save_leaves_no_temporary_behind() {
        let dir = temp_dir("atomic");
        let file = dir.join("conf.txt");
        fs::write(&file, "before").unwrap();

        let mut document = Document::open(&file).unwrap();
        document.buffer = Buffer::from_text("after");
        document.save().unwrap();

        assert_eq!(fs::read_to_string(&file).unwrap(), "after");

        // The temp file lives in the same directory; it must be gone.
        let leftovers: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "a temporary file was left behind");
    }

    #[test]
    fn saving_reanchors_the_disk_hash() {
        let dir = temp_dir("reanchor");
        let file = dir.join("e.txt");
        fs::write(&file, "v1").unwrap();

        let mut document = Document::open(&file).unwrap();
        document.buffer.insert_char(0, 2, '!');
        document.save().unwrap();

        // After saving, our contents ARE the disk contents: no false alarms.
        assert_eq!(document.disk_state(), DiskState::Unchanged);
    }

    // ── Line endings ────────────────────────────────────────────

    #[test]
    fn detects_the_terminator_the_file_actually_uses() {
        assert_eq!(LineEnding::detect("a\nb\n"), LineEnding::Lf);
        assert_eq!(LineEnding::detect("a\r\nb\r\n"), LineEnding::Crlf);
        assert_eq!(LineEnding::detect("no newline at all"), LineEnding::Lf);
        // Mixed: not every newline is a CRLF, so we refuse to guess and leave
        // the file alone rather than rewriting the odd lines out.
        assert_eq!(LineEnding::detect("a\r\nb\nc"), LineEnding::Lf);
    }

    #[test]
    fn a_crlf_file_is_read_without_stray_carriage_returns() {
        let dir = temp_dir("crlf_read");
        let file = dir.join("windows.txt");
        fs::write(&file, "uno\r\ndos\r\ntres").unwrap();

        let document = Document::open(&file).unwrap();

        assert_eq!(document.line_ending(), LineEnding::Crlf);
        assert_eq!(document.buffer.line_count(), 3);
        assert_eq!(document.buffer.line(0), Some("uno"));
        assert_eq!(document.buffer.line(1), Some("dos"));
        // The buffer holds no terminators at all — column arithmetic upstream
        // depends on that.
        assert!(document.buffer.iter_lines().all(|line| !line.contains('\r')));
    }

    #[test]
    fn a_crlf_file_stays_crlf_when_saved() {
        let dir = temp_dir("crlf_save");
        let file = dir.join("config.ini");
        fs::write(&file, "clave=1\r\notra=2").unwrap();

        let mut document = Document::open(&file).unwrap();
        document.buffer.insert_char(0, 7, '0');
        document.save().unwrap();

        let written = fs::read_to_string(&file).unwrap();
        assert_eq!(written, "clave=10\r\notra=2");
    }

    #[test]
    fn an_lf_file_never_grows_carriage_returns() {
        let dir = temp_dir("lf_save");
        let file = dir.join("script.sh");
        fs::write(&file, "#!/bin/sh\necho hola").unwrap();

        let mut document = Document::open(&file).unwrap();
        assert_eq!(document.line_ending(), LineEnding::Lf);

        document.buffer.insert_char(1, 4, '!');
        document.save().unwrap();

        let written = fs::read_to_string(&file).unwrap();
        assert!(!written.contains('\r'));
        assert_eq!(written, "#!/bin/sh\necho! hola");
    }

    #[test]
    fn opening_a_crlf_file_and_saving_it_untouched_is_a_byte_identical_write() {
        // The point of the whole exercise: a one-key edit must not turn into a
        // diff against every line, and a file left alone must hash the same.
        let dir = temp_dir("crlf_identity");
        let file = dir.join("hosts");
        let original = "127.0.0.1 localhost\r\n::1 localhost\r\n";
        fs::write(&file, original).unwrap();

        let mut document = Document::open(&file).unwrap();
        assert!(!document.is_modified());
        assert_eq!(document.disk_state(), DiskState::Unchanged);

        document.save().unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), original);
    }
}
