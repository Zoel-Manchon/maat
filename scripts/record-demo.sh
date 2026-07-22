#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v vhs >/dev/null 2>&1; then
  echo "VHS is required: https://github.com/charmbracelet/vhs" >&2
  exit 1
fi

vhs demo/maat.tape
