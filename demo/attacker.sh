#!/usr/bin/env bash
# Safe local tamper simulator used only by the Maat demo.
# It does not exploit anything: it waits, appends one clearly marked line to
# the chosen demo file, records what it did, and exits.
set -euo pipefail

TARGET="${1:-demo/sample.txt}"
LOG="${2:-/tmp/maat-attacker.log}"
DELAY="${3:-30}"

printf '[simulated attacker] armed for %s (delay: %ss)\n' "$TARGET" "$DELAY" >"$LOG"
sleep "$DELAY"
printf '\n# tampered externally by the safe demo attacker\n' >>"$TARGET"
printf '[simulated attacker] appended one line to %s\n' "$TARGET" >>"$LOG"
