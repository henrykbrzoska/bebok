#!/usr/bin/env node
/**
 * Build the Firefox flavour of the Bebok Companion extension.
 *
 * `client/chrome-extension/` is the single source of truth: the four scripts
 * are browser-agnostic (`chrome.*` with callbacks works in Firefox; Firefox
 * returns promises when the callback is omitted), so the only real difference
 * is the manifest. Chrome needs `background.service_worker` (MV3), Firefox
 * needs `background.scripts` (an event page) plus a `browser_specific_settings`
 * gecko id — a directory can hold exactly one `manifest.json`, so Firefox gets
 * a generated copy under `dist/firefox-extension/`.
 *
 * Usage:
 *   node scripts/build-firefox-extension.mjs             # -> dist/firefox-extension
 *   node scripts/build-firefox-extension.mjs --out DIR   # custom output dir
 *   node scripts/build-firefox-extension.mjs --zip       # also write a .zip (web-ext)
 *
 * Loading it: `about:debugging#/runtime/this-firefox` -> "Load Temporary
 * Add-on…" -> pick `dist/firefox-extension/manifest.json`. Temporary add-ons
 * vanish on browser restart; for a permanent install use
 * `npx web-ext sign --source-dir dist/firefox-extension` (needs AMO keys) or
 * `npx web-ext run --source-dir dist/firefox-extension` for a live session.
 */
import { cp, mkdir, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';

const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const SRC = path.join(ROOT, 'client', 'chrome-extension');

/** Files/dirs shared verbatim between the Chrome and Firefox builds. */
const SHARED = [
  'background.js',
  'content.js',
  'popup.html',
  'popup.js',
  'popup.css',
  'options.html',
  'options.js',
  'README.md',
  'icons',
];

function parseArgs(argv) {
  const args = { out: path.join(ROOT, 'dist', 'firefox-extension'), zip: false };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (arg === '--out') {
      const value = argv[++i];
      if (!value) throw new Error('--out needs a directory');
      args.out = path.resolve(ROOT, value);
    } else if (arg === '--zip') {
      args.zip = true;
    } else if (arg === '-h' || arg === '--help') {
      console.log(
        'usage: node scripts/build-firefox-extension.mjs [--out DIR] [--zip]'
      );
      process.exit(0);
    } else {
      throw new Error('unknown argument: ' + arg);
    }
  }
  return args;
}

async function readJson(file) {
  return JSON.parse(await readFile(file, 'utf8'));
}

async function main() {
  const args = parseArgs(process.argv.slice(2));

  const chromeManifest = await readJson(path.join(SRC, 'manifest.json'));
  const firefoxManifest = await readJson(path.join(SRC, 'manifest.firefox.json'));
  // One version for both builds: the Chrome manifest stays authoritative.
  firefoxManifest.version = chromeManifest.version;

  await rm(args.out, { recursive: true, force: true });
  await mkdir(args.out, { recursive: true });

  for (const entry of SHARED) {
    await cp(path.join(SRC, entry), path.join(args.out, entry), {
      recursive: true,
    });
  }

  await writeFile(
    path.join(args.out, 'manifest.json'),
    JSON.stringify(firefoxManifest, null, 2) + '\n'
  );

  const name = `bebok-companion-firefox-${firefoxManifest.version}`;
  const manifestPath = path.join(args.out, 'manifest.json');
  if (firefoxManifest.background && firefoxManifest.background.service_worker) {
    throw new Error(
      'manifest.firefox.json must not declare background.service_worker — Firefox rejects it'
    );
  }
  console.log(`Firefox extension ready: ${args.out}`);
  console.log(`  version: ${firefoxManifest.version}`);
  console.log(`  load:    about:debugging#/runtime/this-firefox -> Load Temporary Add-on…`);
  console.log(`           pick ${manifestPath}`);
  console.log(`           (NOT ${path.join(SRC, 'manifest.json')} — that one is the Chrome build)`);
  console.log(`  run:     npx web-ext run --source-dir ${args.out}`);

  if (args.zip) {
    const zipFile = path.join(ROOT, 'dist', `${name}.zip`);
    // `zip` is present on Linux/macOS; Windows users can use Compress-Archive.
    const res = spawnSync('zip', ['-qr', zipFile, '.'], {
      cwd: args.out,
      stdio: 'inherit',
    });
    if (res.error || res.status !== 0) {
      console.warn(
        `  zip skipped (no usable \`zip\` binary); pack ${path.relative(ROOT, args.out)} manually`
      );
    } else {
      console.log(`  zip:  ${path.relative(ROOT, zipFile)}`);
    }
  }
}

main().catch((err) => {
  console.error(err?.message || err);
  process.exit(1);
});
