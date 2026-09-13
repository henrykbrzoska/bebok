#!/usr/bin/env node
// bump-version.mjs — set the release version in every manifest at once.
//
//   cd client && npm run version:bump -- 1.5.0
//
// Edits (all must agree, .github/workflows/release.yml preflight enforces it):
//   client/package.json             "version"
//   client/package-lock.json        root "version" + packages[""].version
//   client/src-tauri/tauri.conf.json "version"   (used for bundle file names)
//   client/src-tauri/Cargo.toml     [package] version
//   client/src-tauri/Cargo.lock     bebok-desktop entry
//   engine/Cargo.toml               [workspace.package] version
//   engine/Cargo.lock               every workspace crate entry (bebok-*)
//
// Lockfiles are patched in place so `cargo build --locked` (used by the
// release workflow) keeps working without a network round-trip.
import { readFileSync, writeFileSync, existsSync, readdirSync } from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const clientDir = dirname(dirname(fileURLToPath(import.meta.url)));
const rootDir = join(clientDir, '..');
const engineDir = join(rootDir, 'engine');
const tauriDir = join(clientDir, 'src-tauri');

const version = process.argv[2];
const SEMVER = /^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/;
if (!version || !SEMVER.test(version)) {
  console.error('usage: npm run version:bump -- <x.y.z[-pre]>');
  process.exit(2);
}

const changed = [];
function rel(p) {
  return relative(rootDir, p).replace(/\\/g, '/');
}
function patch(path, fn) {
  const before = readFileSync(path, 'utf8');
  const after = fn(before);
  if (after === before) {
    console.log(`  = ${rel(path)} (already ${version})`);
    return;
  }
  writeFileSync(path, after);
  changed.push(rel(path));
  console.log(`  * ${rel(path)}`);
}

// JSON files: string-replace the top-level "version" so formatting is preserved.
function jsonTopLevelVersion(text) {
  const re = /^(\s*"version"\s*:\s*")([^"]+)(")/m;
  if (!re.test(text)) throw new Error('no top-level "version" field');
  return text.replace(re, `$1${version}$3`);
}

// TOML: version = "..." inside the given [section].
function tomlSectionVersion(text, section) {
  const lines = text.split('\n');
  let inSection = false;
  let done = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (/^\s*\[/.test(line)) inSection = line.trim() === `[${section}]`;
    if (inSection && /^\s*version\s*=/.test(line)) {
      lines[i] = line.replace(/^(\s*version\s*=\s*")[^"]*(")/, `$1${version}$2`);
      done = true;
      break;
    }
  }
  if (!done) throw new Error(`no version in [${section}]`);
  return lines.join('\n');
}

// Cargo.lock: bump `version` of the [[package]] entries whose name matches
// (CRLF-safe: lockfiles may be checked out with Windows line endings).
function cargoLockVersions(text, isWorkspaceCrate) {
  const re = /(\[\[package\]\]\r?\nname = "([^"]+)"\r?\nversion = ")[^"]*(")/g;
  let hits = 0;
  const out = text.replace(re, (all, head, name, tail) => {
    if (!isWorkspaceCrate(name)) return all;
    hits++;
    return `${head}${version}${tail}`;
  });
  if (hits === 0) throw new Error('no matching [[package]] entries found');
  return out;
}

// Workspace crate names from engine/crates/*/Cargo.toml.
function engineCrateNames() {
  const engineToml = readFileSync(join(engineDir, 'Cargo.toml'), 'utf8');
  const names = new Set();
  for (const dir of readdirSync(join(engineDir, 'crates'), { withFileTypes: true })) {
    if (!dir.isDirectory()) continue;
    const toml = join(engineDir, 'crates', dir.name, 'Cargo.toml');
    if (!existsSync(toml)) continue;
    const t = readFileSync(toml, 'utf8');
    const m = t.match(/^\[package\][^[]*?^name\s*=\s*"([^"]+)"/ms);
    if (m && /version\.workspace\s*=\s*true/.test(t)) names.add(m[1]);
  }
  if (!/\[workspace\.package\]/.test(engineToml)) throw new Error('engine/Cargo.toml has no [workspace.package]');
  return names;
}
console.log(`bumping to ${version}`);

patch(join(clientDir, 'package.json'), jsonTopLevelVersion);
patch(join(clientDir, 'package-lock.json'), (text) => {
  // root "version" (line 3) and packages[""].version — both are the first two
  // "version" occurrences that sit directly under the root / "" package.
  const rootRe = /^(\{\s*\n\s*"name"\s*:\s*"[^"]*",\s*\n\s*"version"\s*:\s*")([^"]+)(")/;
  const pkgRe = /("packages"\s*:\s*\{\s*\n\s*""\s*:\s*\{\s*\n\s*"name"\s*:\s*"[^"]*",\s*\n\s*"version"\s*:\s*")([^"]+)(")/;
  if (!rootRe.test(text) || !pkgRe.test(text)) throw new Error('unexpected package-lock.json layout');
  return text.replace(rootRe, `$1${version}$3`).replace(pkgRe, `$1${version}$3`);
});
patch(join(tauriDir, 'tauri.conf.json'), jsonTopLevelVersion);
patch(join(tauriDir, 'Cargo.toml'), (t) => tomlSectionVersion(t, 'package'));
patch(join(tauriDir, 'Cargo.lock'), (t) => cargoLockVersions(t, (n) => n === 'bebok-desktop'));
patch(join(engineDir, 'Cargo.toml'), (t) => tomlSectionVersion(t, 'workspace.package'));
const crates = engineCrateNames();
patch(join(engineDir, 'Cargo.lock'), (t) => cargoLockVersions(t, (n) => crates.has(n)));

console.log(changed.length ? `\n${changed.length} file(s) updated.` : '\nnothing to do.');
console.log(`
next:
  git add -A && git commit -m "chore: bump version to ${version}"
  git tag ${version} && git push origin main ${version}
  (see scripts/release.md)`);
