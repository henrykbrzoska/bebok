#!/usr/bin/env node
// Copies the release engine binary into src-tauri/binaries with the target
// triple suffix Tauri's externalBin expects, e.g.
//   src-tauri/binaries/bebok-server-x86_64-unknown-linux-gnu
// Run after `cargo build --release -p bebok-server` in engine/.
import { execSync } from 'node:child_process';
import { cpSync, mkdirSync, existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const clientDir = dirname(dirname(fileURLToPath(import.meta.url)));
const engineDir = join(clientDir, '..', 'engine');

// Windows builds produce `bebok-server.exe`; Tauri expects the sidecar to be
// named `<name>-<target-triple>.exe` there.
const isWindows = process.platform === 'win32';
const exe = isWindows ? '.exe' : '';
const source = join(engineDir, 'target', 'release', `bebok-server${exe}`);

const host = execSync('rustc -vV', { encoding: 'utf8' }).match(/host: (\S+)/)?.[1];
if (!host) {
  console.error('could not determine the rustc host triple');
  process.exit(1);
}
if (!existsSync(source)) {
  console.error(`engine binary not found at ${source}`);
  console.error('build it first:  cd engine && cargo build --release');
  process.exit(1);
}

const outDir = join(clientDir, 'src-tauri', 'binaries');
mkdirSync(outDir, { recursive: true });
const out = join(outDir, `bebok-server-${host}${exe}`);
cpSync(source, out);
console.log(`sidecar copied -> ${out}`);
