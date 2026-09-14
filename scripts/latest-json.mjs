#!/usr/bin/env node
// latest-json.mjs — compose the Tauri updater manifest (`latest.json`) from
// the collected release artifacts. Zero deps, Node >= 20.
//
//   node scripts/latest-json.mjs --artifacts <dir> --version <x.y.z> --tag <tag> \
//        --repo <owner/name> [--changelog CHANGELOG.md] [--out <dir>/latest.json]
//
// The desktop shell (`tauri-plugin-updater`) fetches
// `https://github.com/<repo>/releases/latest/download/latest.json` and looks
// its platform up as `<os>-<arch>-<installer>` first, then `<os>-<arch>`.
// Every entry needs the bundle AND its minisign `.sig` (produced by CI only
// when TAURI_SIGNING_PRIVATE_KEY is set) - a missing one fails the run, so a
// release never ships a manifest the installed apps cannot use.

import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

function arg(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  if (index === -1 || index + 1 >= process.argv.length) {
    if (fallback !== undefined) return fallback;
    throw new Error(`missing --${name}`);
  }
  return process.argv[index + 1];
}

const artifacts = arg('artifacts');
const version = arg('version');
const tag = arg('tag');
const repo = arg('repo');
const changelog = arg('changelog', null);
const out = arg('out', join(artifacts, 'latest.json'));

/** Updater target key -> bundle file name (the `.sig` sits next to it). */
function platformFiles(version) {
  const nsis = `bebok_${version}_x64-setup.exe`;
  const msi = `bebok_${version}_x64_en-US.msi`;
  const appimage = `bebok_${version}_amd64.AppImage`;
  const deb = `bebok_${version}_amd64.deb`;
  return {
    'windows-x86_64-nsis': nsis,
    'windows-x86_64-msi': msi,
    'windows-x86_64': nsis,
    'linux-x86_64-appimage': appimage,
    'linux-x86_64-deb': deb,
    'linux-x86_64': appimage,
    'darwin-aarch64': `bebok_${version}_aarch64.app.tar.gz`,
    'darwin-x86_64': `bebok_${version}_x64.app.tar.gz`,
  };
}

/** The `## <version> — <date>` section of CHANGELOG.md, without its heading. */
function changelogNotes(markdown, version) {
  const lines = markdown.split(/\r?\n/);
  const start = lines.findIndex((line) => new RegExp(`^## ${version.replace(/\./g, '\\.')}\\b`).test(line));
  if (start === -1) return null;
  const rest = lines.slice(start + 1);
  const end = rest.findIndex((line) => /^## /.test(line));
  return rest.slice(0, end === -1 ? rest.length : end).join('\n').trim() || null;
}

function buildManifest({ version, tag, repo, artifacts, notes }) {
  const platforms = {};
  const missing = [];
  for (const [target, file] of Object.entries(platformFiles(version))) {
    const bundle = join(artifacts, file);
    const sig = `${bundle}.sig`;
    if (!existsSync(bundle)) missing.push(file);
    if (!existsSync(sig)) missing.push(`${file}.sig`);
    if (!existsSync(bundle) || !existsSync(sig)) continue;
    platforms[target] = {
      signature: readFileSync(sig, 'utf8').trim(),
      url: `https://github.com/${repo}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(file)}`,
    };
  }
  if (missing.length > 0) {
    throw new Error(
      `latest.json: missing updater artifacts (is TAURI_SIGNING_PRIVATE_KEY set?):\n  ${[...new Set(missing)].join('\n  ')}`,
    );
  }
  return {
    version,
    notes: notes ?? `Bebok ${version}`,
    pub_date: new Date().toISOString(),
    platforms,
  };
}

const notes = changelog && existsSync(changelog) ? changelogNotes(readFileSync(changelog, 'utf8'), version) : null;
const manifest = buildManifest({ version, tag, repo, artifacts, notes });
writeFileSync(out, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`wrote ${out} (${Object.keys(manifest.platforms).length} targets)`);
