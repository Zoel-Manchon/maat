# Maat technical review

## Assessment

Maat has a small, comprehensible base with a clear value proposition. The
`core`/`ui` split lets the domain be tested without a terminal; the buffer uses
character indices to avoid invalid UTF-8 slicing; and the SHA-256 check before
saving is what separates the project from a generic modal-editor demo.

Version 0.2.0 already delivers a coherent experience for small and medium files:
modal navigation, search, history, a line register, help, scrolling and
integrity. It should not yet be presented as a full Vim replacement or as a
forensic tool.

## What landed

### Identity and experience

- **Cinder Amber** theme with no phosphor green: ink, ivory, copper, amber, coral.
- Welcome screen for new buffers.
- Built-in help panel via `?` and `:help`.
- Current-line highlight, search highlighting, status bar with metrics.
- Absolute or relative line numbers.
- Recommended typeface **Departure Mono**, configured from the terminal.

### Editing

- Incremental search `/`, highlighting and `n` / `N` navigation with wrap.
- Undo and redo via snapshots capped at 512 states.
- Basic line register: `yy`, `dd`, `p` and `P`.
- `:info` and `:check` commands.
- Optional quick save with `Ctrl-s`.

### Fixes and performance

- `:wq!` no longer abandons the session when the write fails.
- `save_as` restores the previous path and disk hash if the target fails.
- The status bar reuses the cached SHA-256 instead of recomputing it every render.
- Tests updated, plus new coverage for history, search and the register.

## Technical priorities

### 1. Atomic saves and metadata — **done in 0.3**

`fs::write` could leave a partial file behind if the process or the machine died
mid-write. `write_atomic` now does the standard dance: temporary file in the
same directory, `flush` + `sync_all`, permissions carried over, atomic rename,
and a directory sync on Unix.

Still open: ownership is not preserved (that needs a libc binding), and it is
still worth distinguishing a deleted file from a permissions error and from a
read error — today several failures all collapse into `DiskState::Missing`.

### 2. Operation-based history

The current undo/redo stores a full buffer copy per mutation. That is simple and
correct for a prototype, but it burns memory and treats every typed character as
an independent action.

The right evolution is an `Edit` model with `apply` and `undo`, grouping an
insertion session into a single transaction. That model unlocks:

- the change operator `c`;
- repeat with `.`;
- macros;
- a recovery journal;
- persistent history.

### 3. Unicode display width and tabs

Counting characters is not the same as counting terminal cells. CJK, emoji,
combining characters and tabs can all desynchronise the cursor, selection and
scroll. This needs a display-width layer and a configurable tab-expansion policy.

### 4. A modal state machine

`pending: Option<char>` is enough for `gg`, `dd` and `yy`, but it does not scale
to Vim's grammar. Worth modelling explicitly:

- operators (`d`, `c`, `y`);
- motions (`w`, `b`, `$`, `gg`);
- counts (`3dd`, `5j`);
- registers;
- visual mode;
- repeat (`.`).

That keeps `handle_normal` from growing into a monolithic `match`.

### 5. Scalable buffer and deferred hashing

`Vec<String>` keeps the code clear and suits this phase. For large files, a rope
or piece table will cut copying and enable a more efficient history. The buffer
hash should also be computed lazily or incrementally, rather than walking the
whole document after every keystroke.

## Roadmap by phase

### 0.3 — shipped

- [x] Atomic saves.
- [x] Structured audit events (JSON / CEF).
- [x] `--verify` non-interactive integrity check.
- [x] `$EDITOR`-safe exit codes.
- [x] 16-colour degradation for appliance consoles.

### Still open for 0.3.x

- Simple substitution `:s/pattern/replacement/`.
- Counts for motions and operations.
- Preserve CRLF/LF line endings.
- Bracketed paste.
- Safe terminal restoration on panic.

### 0.4

- Visual mode.
- Composable operators and motions.
- Optional system clipboard.
- TOML configuration for theme, tabs and numbers.
- File picker and multiple buffers.

### 0.5

- Rope or piece table.
- Incremental syntax highlighting.
- Recovery journal / swap file.
- Signed binaries and cross-platform releases.

## Product direction

The differentiator should not be "implement all of Vim", but to offer a compact,
auditable modal editor that is genuinely useful on servers, appliances and
sensitive workflows. The strongest product line is deepening integrity
awareness:

- surface the hash as read, the current hash and the external hash;
- diff before overwriting a conflict;
- a history of saves;
- read-only mode when permissions or ownership change;
- structured audit events;
- optional document signing or verification.

## Emberwall integration path

Maat is meant to ship as the default editor inside an Emberwall appliance
image. That target imposes concrete constraints, and each one is also a feature
worth having on its own.

### Constraints the appliance imposes

- **Static, small binary.** Emberwall has no package manager and a few-MB
  userland. Build for `x86_64-unknown-linux-musl` and
  `aarch64-unknown-linux-musl` so the binary carries no dynamic libc
  dependency, and keep the dependency tree short — `ratatui`, `crossterm` and
  `sha2` is already a good place to be.
- **No assumptions about the terminal.** Appliance consoles are often a plain
  Linux VT with 16 colours and no true colour. Cinder Amber should degrade
  gracefully: detect `COLORTERM` and fall back to a 16-colour variant rather
  than rendering mud.
- **Read-only root.** An immutable userland means config and swap files cannot
  live next to the binary. Respect `XDG_CONFIG_HOME` / `XDG_STATE_HOME` and
  degrade cleanly when both are unwritable.
- **No `$EDITOR` chain.** If maat is the appliance's only editor, `visudo`,
  `crontab -e` and friends will invoke it. That means honouring the convention
  of exiting non-zero when the user aborts, so those tools don't commit a
  half-edited file.

### Where the integrity story pays off

On a hardened appliance, editing a config file is a security-relevant act. The
features that make maat worth shipping there:

- **Audit events.** Emit a structured line (JSON, or CEF to match phosphor's
  export) on every save: path, hash before, hash after, timestamp, uid. Piped
  into the appliance's log stream, that yields a tamper-evident record of who
  changed which config and to what.
- **Pairing with phosphor.** phosphor watches the filesystem and holds an
  HMAC-signed baseline; maat is what legitimately changes those files. Teach
  maat to re-anchor a phosphor baseline entry after a save — with the key
  supplied by the operator — and the two tools stop fighting: authorised edits
  update the baseline, unauthorised ones still trip the alarm.
- **Read-only enforcement.** When the file is owned by root and the session is
  not, open read-only and say so, rather than failing at write time.
- **`--verify` mode.** A non-interactive flag that prints a file's SHA-256 and
  exits, so the appliance's own boot scripts can use the same binary to check
  config integrity without launching a UI.

### Suggested build target

```
cargo build --release --target x86_64-unknown-linux-musl
```

with a Buildroot package definition that drops the resulting binary into
`/usr/bin/maat` and symlinks `/usr/bin/vi` to it.
