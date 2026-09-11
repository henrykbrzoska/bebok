#!/usr/bin/env node
// copy-sidecar.mjs — odporna kopia sidecara (fix na Os 5 PermissionDenied).
// Czysty Node:fs — zamiennik shellowych cp/copy/del/icacls.
// Użycie: node scripts/copy-sidecar.mjs
//
// Naprawa Os 5 w tauri-build (copy_binaries: remove_file(dest).unwrap()):
// tauri-build kopiuje binaries/bebok-server-<triple>.exe -> target/release/bebok-server.exe
// i najpierw robi remove_file(dest). Dlatego odblokowujemy OBA pliki:
//   1. binaries/bebok-server-<triple>.exe (cel tej kopii),
//   2. target/release/bebok-server.exe (+ .pdb) — cel kopii tauri-build.
// Jesli ktorys jest zablokowany (dziala bebok-desktop.exe / bebok-server.exe,
// skan AV, read-only), padamy WCZESNIE z czytelnym komunikatem zamiast
// cryptic panic w build-scripcie.
import { execSync } from 'node:child_process';
import { cpSync, mkdirSync, existsSync, chmodSync, rmSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const clientDir = dirname(dirname(fileURLToPath(import.meta.url)));
const engineDir = join(clientDir, '..', 'engine');
const tauriDir = join(clientDir, 'src-tauri');

const isWindows = process.platform === 'win32';
const exe = isWindows ? '.exe' : '';
const source = join(engineDir, 'target', 'release', `bebok-server${exe}`);

function rustcTriple() {
  try {
    const m = execSync('rustc -vV', { encoding: 'utf8' }).match(/host: (\S+)/);
    if (m) return m[1];
  } catch { /* fallback ponizej */ }
  const arch = process.arch === 'x64' ? 'x86_64' : process.arch === 'arm64' ? 'aarch64' : process.arch;
  if (isWindows) return `${arch}-pc-windows-msvc`;
  if (process.platform === 'darwin') return `${arch}-apple-darwin`;
  return `${arch}-unknown-linux-gnu`;
}

const host = rustcTriple();
if (!existsSync(source)) {
  console.error(`engine binary not found at ${source}`);
  console.error('build it first:  cd engine && cargo build --release');
  process.exit(1);
}

function unlock(path) {
  if (!existsSync(path)) return true;
  try {
    chmodSync(path, 0o666);
  } catch { /* ignoruj — plik moze byc zablokowany */ }
  try {
    rmSync(path, { force: true });
    return true;
  } catch (e) {
    console.error(`cannot remove locked file ${path}: ${e.message}`);
    console.error('-> zamknij bebok-desktop.exe / bebok-server.exe (Menedzer zadan)');
    console.error('   i sprobuj ponownie. To jest przyczyna Os 5 PermissionDenied');
    console.error('   w tauri-build (copy_binaries: remove_file().unwrap()).');
    return false;
  }
}

const outDir = join(tauriDir, 'binaries');
mkdirSync(outDir, { recursive: true });
const out = join(outDir, `bebok-server-${host}${exe}`);

// 1. Odblokuj cel w binaries/ (Windows: read-only / blokada .exe / skan AV).
if (!unlock(out)) process.exit(1);

try {
  cpSync(source, out);
} catch (e) {
  console.error(`copy failed: ${e.message}`);
  process.exit(1);
}

// 2. Pre-clean celu tauri-build, zeby jego remove_file(dest) nie panikowal.
//    Brak tego kroku = Os 5 w lib.rs:80 przy `npm run tauri:build`.
if (isWindows) {
  const dest = join(tauriDir, 'target', 'release', `bebok-server${exe}`);
  const pdb = join(tauriDir, 'target', 'release', 'bebok-server.pdb');
  if (!unlock(dest)) process.exit(1);
  unlock(pdb); // .pdb nieblokujacy — ignoruj wynik
}

const bytes = statSync(out).size;
console.log(`sidecar copied -> ${out} (${bytes} bytes)`);
