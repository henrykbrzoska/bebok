#!/usr/bin/env node
// clean-tauri-target.mjs — zamiennik shellowych `rm -rf`, `del`, `rmdir /s`
// (te polecenia padaly tu jako `command failed`). Czysty Node:fs, dziala w
// PowerShell i cmd bez bash/sh.
// Uzycie: node scripts/clean-tauri-target.mjs [--bundle-only]
//
// Naprawa: przed rmSync robimy chmod-pass (666 pliki / 777 katalogi),
// bo artefakty bundla (nsis/wix/resources) potrafia zostac read-only
// i wtedy rmSync na Windows pada Os 5 PermissionDenied.
import { rmSync, existsSync, chmodSync, readdirSync, statSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const clientDir = dirname(dirname(fileURLToPath(import.meta.url)));
const tauriDir = join(clientDir, 'src-tauri');
const bundleOnly = process.argv.includes('--bundle-only');

// Uwaga: NIE ruszamy binaries/ (sidecar — robi to copy-sidecar.mjs z unlockiem)
// ani gen/ (tauri-build je odtwarza). Nie ruszamy tez target/release/*.exe —
// to robi unlock() w copy-sidecar.mjs z czytelnym bledem zamknij-proces.
const targets = bundleOnly
  ? [join(tauriDir, 'target', 'release', 'bundle')]
  : [
      join(tauriDir, 'target', 'release', 'bundle'),
      join(tauriDir, 'target', 'release', 'nsis'),
      join(tauriDir, 'target', 'release', 'wix'),
      join(tauriDir, 'target', 'release', 'resources'),
    ];

// Rekurencyjny chmod przed usunieciem — zdejmuje read-only (Os 5).
function writablePass(p) {
  let st;
  try {
    st = statSync(p);
  } catch {
    return;
  }
  try {
    chmodSync(p, st.isDirectory() ? 0o777 : 0o666);
  } catch { /* zablokowany — rmSync i tak sprobuje i zglosi */ }
  if (st.isDirectory()) {
    let kids = [];
    try {
      kids = readdirSync(p);
    } catch {
      return;
    }
    for (const k of kids) writablePass(join(p, k));
  }
}

let failed = false;
for (const p of targets) {
  try {
    if (existsSync(p)) {
      writablePass(p);
      rmSync(p, { recursive: true, force: true });
      console.log(`removed ${p}`);
    } else {
      console.log(`skip (missing) ${p}`);
    }
  } catch (e) {
    failed = true;
    console.error(`FAIL ${p}: ${e.message}`);
    console.error('-> zamknij bebok-desktop.exe/bebok-server.exe i sprobuj ponownie.');
  }
}
if (failed) process.exitCode = 1;
else console.log('done.');
