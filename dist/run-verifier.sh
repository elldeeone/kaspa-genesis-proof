#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BIN_PATH="$SCRIPT_DIR/genesis-proof"

if [[ ! -x "$BIN_PATH" ]]; then
  echo "Error: $BIN_PATH not found or not executable"
  echo "Make sure this script is next to the genesis-proof binary."
  read -r -p "Press Enter to exit..." _
  exit 1
fi

if [[ $# -eq 0 ]]; then
  "$BIN_PATH" --node-type auto --pause-on-exit
else
  "$BIN_PATH" "$@"
fi
