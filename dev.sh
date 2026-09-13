#!/usr/bin/env bash
# Bebok dev launcher (Linux / macOS). Builds the engine, starts it, starts
# `ng serve` and prints/opens a client URL with the engine token wired in.
#   ./dev.sh                 -> npm run full-build-dev
#   ./dev.sh tauri           -> same with --tauri (desktop shell via `tauri dev`)
#   ./dev.sh --open --port 8812 --client-port 4790 --no-auth   (any bebok.mjs flag)
# Ctrl+C stops the engine and the dev server. See `node scripts/bebok.mjs --help`.
# (If this file is not executable: `chmod +x dev.sh`.)
set -euo pipefail
cd "$(dirname "$0")"
exec node scripts/bebok.mjs full-build-dev "$@"
