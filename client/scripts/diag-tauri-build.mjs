#!/usr/bin/env node
// diag-tauri-build.mjs — zamiennik WSZYSTKICH failing shell-prob w tym watku.
// Uzycie (PowerShell, bez bash/sh):  node scripts/diag-tauri-build.mjs
// Sprawdza wylacznie Node:fs/path/os — zero `pwd`, `ls`, `sed`, `icacls`,
// `tasklist`, `netstat`, `dir`, `type`. Wynik to JSON na stdout.
import { accessSync, constants, existsSync, statSync, readdirSync } from 'node:fs';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { platform, arch } from 'node:os';

const clientDir = dirname(dirname(fileURLToPath(import.meta.url)));
const tauriDir = join(clientDir, 'src-tauri');
const out = { ok: true, notes: [], checks: {} };

function check(name, p, want = 'read') {
  try {
    const st = statSync(p);
    const isDir = st.isDirectory();
    accessSync(p, want === 'write' ? constants.W_OK : constants.R_OK);
    out.checks[name] = { path: p, exists: true, isDir, size: isDir ? null : st.size, access: 'ok' };
  } catch (e) {
    out.ok = false;
    out.checks[name] = { path: p, exists: existsSync(p), access: 'FAIL', error: `${e.code ?? ''} ${e.message}` };
    out.notes.push(`${name}: ${e.message}`);
  }
}

// Triple per-OS (wczesniej hardcoded windows — padal na linux/mac).
function triple() {
  const a = arch() === 'x64' ? 'x86_64' : arch() === 'arm64' ? 'aarch64' : arch();
  if (platform() === 'win32') return `${a}-pc-windows-msvc`;
  if (platform() === 'darwin') return `${a}-apple-darwin`;
  return `${a}-unknown-linux-gnu`;
}
const tri = triple();
const exe = platform() === 'win32' ? '.exe' : '';

// 1. Konfig i build.rs (zamiast: type/cat/sed tauri.conf.json, Cargo.toml, build.rs)
check('tauri.conf', join(tauriDir, 'tauri.conf.json'));
check('cargo.toml', join(tauriDir, 'Cargo.toml'));
check('build.rs', join(tauriDir, 'build.rs'));
check('capabilities', join(tauriDir, 'capabilities'));
check('capabilities/default.json', join(tauriDir, 'capabilities', 'default.json'));
check('icons', join(tauriDir, 'icons'));
check('icons/icon.png', join(tauriDir, 'icons', 'icon.png'));
check('icons/icon.ico', join(tauriDir, 'icons', 'icon.ico'));
check('icons/icon.icns', join(tauriDir, 'icons', 'icon.icns'));

// 2. frontendDist (zamiast: ls/dir dist/...)
// tauri.conf -> build.frontendDist == "../dist/bebok/browser"
check('frontendDist', join(tauriDir, '..', 'dist', 'bebok', 'browser'));
check('dist-root', join(clientDir, 'dist', 'bebok'));

// 3. externalBin (zamiast: dir binaries / icacls *.exe)
// Tauri oczekuje: binaries/bebok-server-<triple>[.exe]
check('binaries-dir', join(tauriDir, 'binaries'));
check('sidecar-exe', join(tauriDir, 'binaries', `bebok-server-${tri}${exe}`));
try {
  const files = readdirSync(join(tauriDir, 'binaries'));
  out.checks['binaries-list'] = { files };
  const strays = files.filter((f) => !f.includes(tri));
  if (strays.length > 0) out.notes.push(`stray files in binaries/ (usun): ${strays.join(', ')}`);
} catch (e) {
  out.ok = false;
  out.notes.push(`binaries list FAIL: ${e.message}`);
}

// 4. Target / OUT_DIR zapisywalnosc (zamiast: ls -l target/... , icacls OUT_DIR)
// tauri-build panikuje lib.rs:80 PermissionDenied (Os 5) gdy nie moze pisac do OUT_DIR
// albo gdy cel target/release/bebok-server.exe jest zablokowany przez dzialajacy proces.
check('target-dir', join(tauriDir, 'target'), 'write');
check('target-release', join(tauriDir, 'target', 'release'), 'write');
// Cel kopii tauri-build (copy_binaries: remove_file(dest).unwrap() -> Os 5 gdy locked).
// Raportujemy read-only / brak dostepu, ale brak pliku to NIE blad (powstaje w buildzie).
try {
  const dest = join(tauriDir, 'target', 'release', `bebok-server${exe}`);
  if (existsSync(dest)) check('target-bebok-server', dest, 'write');
  else out.checks['target-bebok-server'] = { path: dest, exists: false, access: 'ok (powstanie w buildzie)' };
} catch (e) {
  out.ok = false;
  out.notes.push(`target-bebok-server FAIL: ${e.message}`);
}

// 5. Wskazowki do PermissionDenied (bez tasklist/netstat — patrz Menedzer zadan)
out.hints = [
  'Jesli bebok-desktop.exe / bebok-server.exe dziala (Menedzer zadan), zamknij go przed buildem — Windows blokuje nadpisanie .exe (Os 5 PermissionDenied w tauri-build).',
  `Jesli antivirus skanuje binaries/*.exe lub target/, dodaj wyjatek na ${tauriDir}.`,
  'Po zamknieciu procesow: node scripts/clean-tauri-target.mjs && node scripts/copy-sidecar.mjs && npm run tauri:build',
];
out.meta = { platform: platform(), arch: arch(), clientDir, triple: tri };
console.log(JSON.stringify(out, null, 2));
if (!out.ok) process.exitCode = 1;
