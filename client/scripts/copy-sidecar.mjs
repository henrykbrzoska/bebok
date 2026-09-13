#!/usr/bin/env node
// copy-sidecar.mjs — odporna kopia sidecara (fix na Os 5 PermissionDenied).
// Czysty Node:fs — zamiennik shellowych cp/copy/del/icacls.
// Użycie: node scripts/copy-sidecar.mjs [--target <triple>]
//   --target <triple>  (lub env BEBOK_TARGET / CARGO_BUILD_TARGET) — nazwa sidecara
//                      i zrodlo: engine/target/<triple>/release/ (cargo build --target).
//                      Bez flagi: host triple z `rustc -vV`, zrodlo engine/target/release/.
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

function parseTargetArg() {
  const argv = process.argv.slice(2);
  const i = argv.indexOf('--target');
  if (i !== -1 && argv[i + 1]) return argv[i + 1];
  const eq = argv.find((a) => a.startsWith('--target='));
  if (eq) return eq.slice('--target='.length);
  return process.env.BEBOK_TARGET || process.env.CARGO_BUILD_TARGET || '';
}

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

const explicitTarget = parseTargetArg();
const host = explicitTarget || rustcTriple();
// `cargo build --release --target <triple>` -> target/<triple>/release/;
// plain `cargo build --release` -> target/release/. Prefer the explicit one.
const candidates = explicitTarget
  ? [
      join(engineDir, 'target', explicitTarget, 'release', `bebok-server${exe}`),
      join(engineDir, 'target', 'release', `bebok-server${exe}`),
    ]
  : [join(engineDir, 'target', 'release', `bebok-server${exe}`)];
const source = candidates.find((p) => existsSync(p));
if (!source) {
  console.error(`engine binary not found at ${candidates.join(' or ')}`);
  console.error(
    explicitTarget
      ? `build it first:  cd engine && cargo build --release --target ${explicitTarget}`
      : 'build it first:  cd engine && cargo build --release',
  );
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
  // With `tauri build --target <triple>` cargo writes to target/<triple>/release/.
  const destDir = explicitTarget
    ? join(tauriDir, 'target', explicitTarget, 'release')
    : join(tauriDir, 'target', 'release');
  const dest = join(destDir, `bebok-server${exe}`);
  const pdb = join(destDir, 'bebok-server.pdb');
  if (!unlock(dest)) process.exit(1);
  unlock(pdb); // .pdb nieblokujacy — ignoruj wynik
}

const bytes = statSync(out).size;
console.log(`sidecar copied ${source} -> ${out} (${bytes} bytes, target ${host})`);
