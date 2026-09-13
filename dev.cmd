@echo off
rem Bebok dev launcher (Windows). Builds the engine, starts it, starts `ng serve`
rem and prints/open a client URL with the engine token already wired in.
rem   dev.cmd                 -> npm run full-build-dev
rem   dev.cmd tauri           -> same with --tauri (desktop shell via `tauri dev`)
rem   dev.cmd --open --port 8812 --client-port 4790 --no-auth   (any bebok.mjs flag)
rem Ctrl+C stops the engine and the dev server. See scripts/bebok.mjs --help.
node "%~dp0scripts\bebok.mjs" full-build-dev %*
