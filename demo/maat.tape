Output assets/maat-demo.gif

Require cargo

Set Shell "bash"
Set Width 1200
Set Height 720
Set FontSize 18
Set Padding 24
Set Margin 18
Set MarginFill "#14090b"
Set BorderRadius 8
Set TypingSpeed 55ms
Set CursorBlink false
Set Theme { "name": "Oxblood", "black": "#14090b", "red": "#b03a48", "green": "#9a7070", "yellow": "#e0a458", "blue": "#b03a48", "magenta": "#c48b94", "cyan": "#d96a78", "white": "#e8d8ce", "brightBlack": "#3a2225", "brightRed": "#f05d6c", "brightGreen": "#9a7070", "brightYellow": "#f5ede6", "brightBlue": "#d96a78", "brightMagenta": "#c48b94", "brightCyan": "#d96a78", "brightWhite": "#f5ede6", "background": "#14090b", "foreground": "#e8d8ce", "selection": "#221114", "cursor": "#f5ede6" }

# Build, restore the fixture, enable auditing and arm a harmless process that
# changes the sample while Maat is open. See demo/README.md.
Hide
Type "cargo build --release --locked"
Enter
Type "cp demo/sample.txt /tmp/maat-sample.bak"
Enter
Type "rm -f /tmp/maat-audit.log /tmp/maat-attacker.log"
Enter
Type "export MAAT_AUDIT_LOG=/tmp/maat-audit.log"
Enter
Type "./demo/attacker.sh demo/sample.txt /tmp/maat-attacker.log 36 >/dev/null 2>&1 &"
Enter
Type "clear"
Enter
Show

# 1. CLI and non-interactive integrity verification.
Type "./target/release/maat --version"
Enter
Sleep 1200ms
Type "./target/release/maat --verify demo/sample.txt"
Enter
Sleep 2200ms
Type "clear"
Enter

# 2. Open the file and enable relative line numbers.
Type "./target/release/maat demo/sample.txt"
Enter
Sleep 1800ms
Type ":set relativenumber"
Enter
Sleep 1300ms

# 3. Modal editing and Vim-inspired motions.
Type "ggjjw"
Sleep 500ms
Type "i"
Type "secure, "
Escape
Sleep 1200ms
Type "G"
Sleep 700ms
Type "o"
Type "Operator note: changes remain visible and auditable."
Escape
Sleep 1200ms

# 4. Incremental search and navigation.
Type "/integrity"
Enter
Sleep 1500ms
Type "n"
Sleep 900ms
Type "N"
Sleep 900ms

# 5. Line register and bounded undo/redo history.
Type "yy"
Sleep 500ms
Type "p"
Sleep 700ms
Type "u"
Sleep 700ms
Ctrl+R
Sleep 700ms
Type "u"
Sleep 700ms
Type "dd"
Sleep 650ms
Type "P"
Sleep 650ms
Type "u"
Sleep 900ms

# 6. Document metadata and the full buffer hash.
Type ":info"
Enter
Sleep 1800ms
Type ":hash"
Enter
Sleep 2200ms

# 7. Atomic save and audit event.
Type ":w"
Enter
Sleep 2400ms

# 8. Create another local edit. The safe background simulator has now changed
#    the disk file underneath Maat.
Type "A"
Type " local revision"
Escape
Sleep 800ms
Type ":w"
Enter
Sleep 3200ms
Type ":check"
Enter
Sleep 2400ms

# 9. Deliberate override after the conflict is visible.
Type ":w!"
Enter
Sleep 2600ms

# 10. Integrated help overlay and clean exit.
Type "?"
Sleep 3200ms
Type "?"
Sleep 500ms
Type ":q"
Enter
Sleep 1300ms

# 11. Show the simulated attack and the machine-readable audit trail.
Type "cat /tmp/maat-attacker.log"
Enter
Sleep 2200ms
Type "tail -n 2 /tmp/maat-audit.log"
Enter
Sleep 3500ms

# Restore the repository fixture exactly as it was.
Hide
Type "cp /tmp/maat-sample.bak demo/sample.txt"
Enter
