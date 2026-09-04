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

### 0.3.x — shipped

- [x] Simple substitution: `:s/old/new/`, `:s/old/new/g`, `:%s/old/new/g`, with
      any punctuation usable as the delimiter so paths need no escaping.
      Matching is literal, like `/` — a pattern that quietly means something
      else is the last thing a sudoers file needs.
- [x] Counts for motions and operations: `3j`, `12G`, `5x`, `2dd`, `3p`. A
      leading `0` still means start-of-line; a `0` inside a count is a digit.
- [x] Preserve CRLF/LF line endings. Detected on open, restored on save, and
      surfaced in `:info`, so editing a Windows config on Linux stays a
      one-line diff instead of a whole-file rewrite.
- [x] Bracketed paste: a pasted block arrives as one event and is inserted as
      text. Before this, pasting into Normal mode ran every character as a
      command.
- [x] Safe terminal restoration on panic: a hook leaves the alternate screen
      and raw mode before the panic message is printed, so an appliance console
      is never left unusable with an unreadable error on it.

### 0.4

- [x] Visual mode: `v` character-wise, `V` line-wise, `o` to swap ends, and
      `d` / `y` / `c` / `p` over the selection. The register now remembers
      whether it holds lines or a character span, so a visual yank pastes
      beside the cursor and a `yy` still opens a new line.
- [x] Composable operators and motions: `d`, `y` and `c` take any motion, with
      a count on either side (`2dw` and `d2w` both work). Exclusive, inclusive
      and line-wise motions are modelled explicitly, so `dw` leaves the
      character it landed on and `d$` takes it — the off-by-one that otherwise
      deletes one character too many, every time. `cw` keeps Vim's irregularity
      and stops at the end of the word.
- [x] Optional system clipboard, over **OSC 52** rather than a clipboard
      crate. `arboard` and friends talk to X11, Wayland or Win32, none of which
      exist on an appliance console reached over SSH; OSC 52 asks the operator's
      *own* terminal to hold the text, which is the clipboard they actually
      wanted. Opt-in via `:set clipboard` or `MAAT_CLIPBOARD=1`, because a few
      old terminals echo the sequence instead of acting on it. Base64 is
      written out rather than pulled in — a dependency for forty lines of table
      lookup is a bad trade in a binary meant to stay small and auditable.
- [x] TOML configuration for theme, tabs and numbers, read from
      `$MAAT_CONFIG` / `$XDG_CONFIG_HOME` / `~/.config` and degrading to the
      defaults when there is none — the read-only-root constraint the appliance
      section calls for. The parser is a hand-written TOML subset rather than
      `toml` + `serde`: a dozen scalars are not worth quadrupling a three-crate
      dependency tree in a binary that has to stay small and auditable. Keys it
      does not recognise are reported on the message line, not swallowed.
- [x] File picker and multiple buffers. `:e` opens, `:bn` / `:bp` / `:bd` /
      `:ls` manage, and two pickers — `Ctrl-p` over the file tree, `:buffers`
      over what is open — filter by substring as you type.

      The live buffer's state stays in `App`'s own fields and switching swaps
      the whole set in and out, rather than routing every `self.cursor` through
      an index. That keeps a thousand lines untouched to change nothing
      observable, and leaves an invariant small enough to assert in one line:
      exactly one slot is `None`, and it is the current one. Cursor, scroll and
      undo history are per buffer; the register, search and keymap are shared,
      which is what a person expects from yanking in one file and pasting in
      another.

### 0.5

- [x] Piece table. A line-oriented one: every line is a `(source, byte range)`
      pair into either the file as it was read or an append buffer.

      The textbook design stores spans of the whole document, and then
      `line(row)` cannot return a `&str` — a line may straddle two pieces —
      which would turn a borrow into an allocation per line per frame in the
      renderer and move 150-odd call sites. One piece per line keeps the borrow
      and the API, at the cost of rewriting an edited line rather than
      splitting a piece. A line is small.

      Measured on the same machine against the `Vec<String>` it replaced, on a
      200 000-line file: opening 11.85 ms to 7.92 ms, a hundred undo snapshots
      1.26 s to 199 ms, and a thousand keystrokes 11.85 us to 27.24 us. The
      snapshot column is the one that matters, because it happens on every
      edit. The keystroke column got 2.3x worse and is reported anyway — at 27
      nanoseconds each, that is the reason it is an acceptable trade and not a
      reason to hide it.
- [x] Incremental syntax highlighting. Hand-written lexers for Rust, TOML,
      shell, JSON, Markdown and INI-style config, detected from the file name
      or the shebang. `syntect` carries Sublime grammars and a regex engine and
      `tree-sitter` compiles a parser per language from C; either is right for
      an editor that highlights anything, and neither fits a four-crate binary
      that ships inside an appliance image.

      Each lexer is a function from `(state entering the line, the line)` to
      `(tokens, state leaving it)`, which is what makes it resumable: the
      highlighter caches the state entering every line, so drawing a window
      costs the lines in it, an edit invalidates from that line down, and a
      block comment opened 500 lines above still colours what is on screen.

      The renderer stopped nesting its passes and now paints syntax, search and
      selection onto a per-character style buffer before coalescing runs into
      spans. Nesting breaks the moment two layers overlap on one character —
      the first has already cut the string and the second cannot reach inside
      it.
- [x] Recovery journal. Unsaved work is mirrored to
      `$XDG_STATE_HOME/maat/swap` every twenty edits and on a clean quit, and
      deleted the moment it is on disk — so a journal that outlives its session
      *means* something. `:recover` loads it back as an unsaved buffer and
      `:discard` drops it; recovery never writes the file by itself, and it
      records the disk hash so a file that changed underneath is flagged rather
      than quietly overwritten. `:q!` clears the journal: discarding is a
      decision, not a crash.
- [x] Cross-platform releases: a tag builds six targets (musl x86_64 and
      aarch64 for the appliance, gnu x86_64, both macOS architectures, and
      Windows MSVC) and publishes them with a `SHA256SUMS` file — an editor
      that sells integrity has no business shipping binaries nobody can verify.
      The musl build is static-pie and around 900 KB.
- Signing proper. Not done, and not fudged: it needs an Apple Developer ID and
  an Authenticode certificate. The release notes say so instead of implying the
  checksums are a signature.

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
