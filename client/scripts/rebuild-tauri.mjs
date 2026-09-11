#!/usr/bin/env node
// rebuild-tauri.mjs — pelny lancuch naprawczy bez `&&` (padal jako `command failed`
// w cmd/PowerShell przy lancuchach shellowych). Czysty Node: sekwencyjnie
// clean -> copy-sidecar -> informacja o tauri build.
// Uzycie: node scripts/rebuild-tauri.mjs [--bundle-only] [--skip-build]
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const clientDir = dirname(dirname(fileURLToPath(import.meta.url)));
const args = process.argv.slice(2);

function run(script, extra = []) {
  const r = spawnSync(process.execPath, [join(clientDir, 'scripts', script), ...extra, ...args.filter((a) => a.startsWith('--'))], {
    stdio: 'inherit',
    cwd: clientDir,
  });
  return r.status ?? 1;
}

const cleanCode = run('clean-tauri-target.mjs');
if (cleanCode !== 0) {
  console.error(`clean-tauri-target.mjs failed (${cleanCode}) — przerwalem przed kopia sidecara.`);
  process.exit(cleanCode);
}

const copyCode = run('copy-sidecar.mjs');
if (copyCode !== 0) {
  console.error(`copy-sidecar.mjs failed (${copyCode}) — NIE odpalaj tauri build, napraw blokade .exe (patrz wyzej).`);
  process.exit(copyCode);
}

if (args.includes('--skip-build')) {
  console.log('skip-build: gotowe (clean + copy OK).');
  process.exit(0);
}

console.log('clean + copy OK. Teraz odpal: npm run tauri:build');
console.log('(sam build zostawiamy w npm, zeby logi tauri-build byly wprost w konsoli.)');
