# Changelog

All notable changes to Maat are documented here.

## [Unreleased]

### Changed

- Rebuilt the GitHub demo to cover the complete feature set and make the
  external-tamper scenario explicit.
- Added `demo/attacker.sh`, a safe local simulator used only to demonstrate
  conflict detection, deliberate override and audit logging.
- Refreshed the screenshot and removed the obsolete PowerShell publishing
  script.

## [0.3.0] - 2026-07-22

### Added

- **Atomic saves.** Writes now go to a temporary file in the same directory,
  are `fsync`ed, inherit the original permissions and are then renamed over the
  target. A crash mid-write can no longer leave a half-written file.
- **Audit events.** With `MAAT_AUDIT_LOG` set, every save appends one
  structured line (JSON by default, CEF via `MAAT_AUDIT_FORMAT=cef`) carrying
  the path, line count, operator and the hashes before and after.
- **`--verify` mode.** Non-interactive integrity check that prints the file's
  SHA-256 in `sha256sum` format plus its disk state, then exits — usable from
  boot scripts without starting a UI.
- **`--help` and `--version`.**
- **Exit codes.** `2` when an edit is abandoned with unsaved changes, so
  `visudo` and `crontab -e` do not install a half-edited file.
- **16-colour fallback.** The palette degrades to indexed ANSI colours when the
  terminal does not advertise true colour, for appliance consoles.

### Changed

- New **Oxblood** theme — ink, wine and bone — replacing Cinder Amber, so the
  editor no longer shares a visual identity with its sibling project `phosphor`.
- Theme colours are now functions rather than constants, to allow the runtime
  true-colour decision.
- README documents the CLI, the audit trail and the architecture as Mermaid
  diagrams.

## [0.2.0] - 2026-07-22

### Added

- Incremental search mode with highlighted matches and `n` / `N` navigation.
- Undo and redo history with `u` and `Ctrl-r`.
- Basic line register with `yy`, `dd`, `p` and `P`.
- Branded welcome screen for new unnamed buffers.
- Integrated quick-reference overlay with `?` and `:help`.
- `:check`, `:info`, relative line numbers and editor metrics.
- GitHub Actions CI, issue templates, Dependabot and VHS demo files.

### Changed

- Expanded the Cinder Amber interface and status line.
- Improved README, package metadata and repository presentation.

### Fixed

- `:wq!` no longer exits when writing fails.
- Failed `:w <path>` restores the previous document path and disk hash.
- Buffer modification status reuses the cached SHA-256 during rendering.

## [0.1.0]

- Initial modal editor prototype.
- Normal, Insert and Command modes.
- UTF-8-safe line buffer.
- SHA-256 external-change detection before saving.
