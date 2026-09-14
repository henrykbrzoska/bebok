#!/usr/bin/env node
// builder.mjs — build release artifacts locally, the way CI does (zero deps, Node >= 20).
//
//   npm run builder                              interactive: pick targets + version
//   npm run builder -- --targets linux,android --version 1.8.0-test1 --yes
//   npm run builder -- --out ~/somewhere                 (default: ~/bebok-dist)
//
// Linux / Windows / Android run in Docker (builder/compose.yml, images built on
// first use). macOS cannot be built in a container: on a macOS host that leg
// runs natively with the same steps (cargo + tauri, signed when ~/.tauri has
// the key). Everything works on a private copy - the checkout keeps its
// version - and lands in <out>/<version>/<platform>/ with CI's file names.

import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, rmSync, cpSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { createInterface } from 'node:readline/promises';
import { fileURLToPath } from 'node:url';

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const COMPOSE = join(ROOT, 'builder', 'compose.yml');
const isWin = process.platform === 'win32';
const isMac = process.platform === 'darwin';

const argv = process.argv.slice(2);
const flag = (name) => {
  const i = argv.indexOf(`--${name}`);
  return i >= 0 ? argv[i + 1] : null;
};
const yes = argv.includes('--yes') || argv.includes('-y');
const OUT = resolve((flag('out') ?? process.env.BEBOK_OUT ?? join(homedir(), 'bebok-dist')).replace(/^~/, homedir()));

const c = {
  bold: (s) => `\x1b[1m${s}\x1b[0m`,
  dim: (s) => `\x1b[2m${s}\x1b[0m`,
  green: (s) => `\x1b[32m${s}\x1b[0m`,
  yellow: (s) => `\x1b[33m${s}\x1b[0m`,
  red: (s) => `\x1b[31m${s}\x1b[0m`,
};
const log = (m) => console.log(m);
function fail(m) {
  console.error(`${c.red('✗')} ${m}`);
  process.exit(1);
}

const TARGETS = [
  { id: 'linux', label: 'Linux x64 (deb, AppImage, portable)', docker: true },
  { id: 'windows', label: 'Windows x64 (NSIS installer, portable; cross-built, experimental)', docker: true },
  { id: 'android', label: 'Android arm64 (APK)', docker: true },
  { id: 'macos', label: `macOS ${process.arch === 'arm64' ? 'Apple silicon' : 'Intel'} (dmg, app.tar.gz)`, docker: false, host: isMac },
];

let rl = null;
async function ask(q, fallback) {
  if (yes) return fallback;
  rl ??= createInterface({ input: process.stdin, output: process.stdout });
  const a = (await rl.question(`${q}${fallback !== undefined ? c.dim(` [${fallback}]`) : ''} `)).trim();
  return a || fallback;
}

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: opts.quiet ? 'pipe' : 'inherit', cwd: opts.cwd ?? ROOT, env: { ...process.env, ...(opts.env ?? {}) }, encoding: 'utf8', windowsHide: true });
  if (r.error) return { ok: false, out: r.error.message };
  return { ok: r.status === 0, out: `${r.stdout ?? ''}${r.stderr ?? ''}` };
}

function manifestVersion() {
  return JSON.parse(readFileSync(join(ROOT, 'client', 'package.json'), 'utf8')).version;
}

/** `<base>-testN` with N = 1 + highest existing test build of that base in <out>. */
function suggestVersion() {
  const base = manifestVersion().split('-')[0];
  let n = 0;
  if (existsSync(OUT)) {
    for (const dir of readdirSync(OUT)) {
      const m = dir.match(new RegExp(`^${base.replace(/\./g, '\\.')}-test(\\d+)$`));
      if (m) n = Math.max(n, Number(m[1]));
    }
  }
  return `${base}-test${n + 1}`;
}

const VERSION_RE = /^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?$/;

async function main() {
  log(c.bold('\nBebok builder') + c.dim(`  (artifacts -> ${OUT})\n`));

  // ---- targets
  let picked = (flag('targets') ?? '').split(',').map((s) => s.trim()).filter(Boolean);
  if (picked.length === 0) {
    TARGETS.forEach((t, i) => {
      const note = !t.docker && !t.host ? c.dim('  (needs a macOS host - skipped)') : '';
      log(`  ${i + 1}) ${t.label}${note}`);
    });
    const answer = await ask('\nWhich targets? (numbers, comma-separated, or "all")', 'all');
    picked =
      answer === 'all'
        ? TARGETS.filter((t) => t.docker || t.host).map((t) => t.id)
        : answer.split(/[,\s]+/).map((n) => TARGETS[Number(n) - 1]?.id).filter(Boolean);
  }
  const targets = TARGETS.filter((t) => picked.includes(t.id));
  if (targets.length === 0) fail('no targets selected');
  const unbuildable = targets.filter((t) => !t.docker && !t.host);
  if (unbuildable.length) fail(`${unbuildable.map((t) => t.id).join(', ')}: only on a macOS host`);

  // ---- version
  let version = flag('version') ?? (await ask('Version for these builds', suggestVersion()));
  version = version.replace(/^v/, '');
  if (!VERSION_RE.test(version)) fail(`'${version}' is not X.Y.Z[-suffix]`);
  const outDir = join(OUT, version);
  mkdirSync(outDir, { recursive: true });

  // ---- tools
  const needDocker = targets.some((t) => t.docker);
  if (needDocker) {
    if (!run('docker', ['compose', 'version'], { quiet: true }).ok) fail('docker compose is not available - start Docker Desktop / OrbStack');
  }
  const secrets = join(homedir(), '.tauri');
  const hasUpdaterKey = existsSync(join(secrets, 'bebok.key'));
  const hasKeystore = existsSync(join(secrets, 'bebok-android.keystore'));
  log(`\n${c.bold('Plan')}`);
  log(`  version   ${version}`);
  log(`  targets   ${targets.map((t) => t.id).join(', ')}`);
  log(`  updater   ${hasUpdaterKey ? c.green('signed (~/.tauri/bebok.key)') : c.yellow('unsigned - not installable as an update')}`);
  if (targets.some((t) => t.id === 'android')) log(`  apk       ${hasKeystore ? c.green('signed (~/.tauri/bebok-android.keystore)') : c.yellow('debug key')}`);
  log(`  output    ${outDir}\n`);
  if (!yes && (await ask('Go? (Y/n)', 'y')).toLowerCase().startsWith('n')) fail('aborted');

  const results = [];
  for (const t of targets) {
    const started = Date.now();
    log(`\n${c.bold(`== ${t.id} ==`)}`);
    let ok;
    if (t.docker) {
      ok = run('docker', ['compose', '-f', COMPOSE, 'run', '--rm', '--build', t.id], {
        env: { VERSION: version, BEBOK_SRC: ROOT, BEBOK_OUT: OUT, BEBOK_SECRETS: secrets },
      }).ok;
    } else {
      ok = buildMacos(version, outDir, secrets);
    }
    results.push({ id: t.id, ok, seconds: Math.round((Date.now() - started) / 1000) });
    if (!ok) log(c.red(`${t.id} failed`));
  }

  // ---- merged checksums (+ updater manifest when every platform is present)
  mergeChecksums(outDir);
  const manifest = run('node', [join(ROOT, 'scripts', 'latest-json.mjs'), '--artifacts', outDir, '--version', version, '--tag', version, '--repo', 'henrykbrzoska/bebok', '--changelog', join(ROOT, 'CHANGELOG.md')], { quiet: true });
  log(`\n${c.bold('Result')}  ${outDir}`);
  for (const r of results) log(`  ${r.ok ? c.green('✓') : c.red('✗')} ${r.id}  ${c.dim(`${r.seconds}s`)}`);
  log(`  ${manifest.ok ? c.green('✓') : c.dim('-')} latest.json ${manifest.ok ? '' : c.dim('(needs all four platforms with .sig - fine for local tests)')}`);
  for (const f of listFiles(outDir)) log(`    ${f}`);
  process.exitCode = results.every((r) => r.ok) ? 0 : 1;
}

/** The macOS leg, natively: same steps as release.yml's macos-* matrix entry. */
function buildMacos(version, outDir, secrets) {
  const triple = process.arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin';
  const platform = process.arch === 'arm64' ? 'macos-arm64' : 'macos-x64';
  const arch = process.arch === 'arm64' ? 'aarch64' : 'x64';
  const work = join(OUT, '.work-macos');
  log(c.dim(`private copy -> ${work}`));
  rmSync(work, { recursive: true, force: true });
  mkdirSync(work, { recursive: true });
  // Tracked + untracked-not-ignored files, like the container's rsync.
  const files = run('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard'], { quiet: true }).out.split('\0').filter(Boolean);
  for (const f of files) {
    if (/^(client\/node_modules|relay\/node_modules|engine\/target|client\/src-tauri\/target)\//.test(f)) continue;
    const src = join(ROOT, f);
    if (!existsSync(src)) continue;
    mkdirSync(dirname(join(work, f)), { recursive: true });
    cpSync(src, join(work, f));
  }
  // Reuse the checkout's node_modules and cargo target dirs (fast, same as `npm run full-build-app`).
  for (const [from, to] of [
    ['client/node_modules', 'client/node_modules'],
    ['engine/target', 'engine/target'],
    ['client/src-tauri/target', 'client/src-tauri/target'],
  ]) {
    if (existsSync(join(ROOT, from))) {
      mkdirSync(dirname(join(work, to)), { recursive: true });
      run('ln', ['-s', join(ROOT, from), join(work, to)], { quiet: true });
    }
  }
  if (!existsSync(join(work, 'client', 'node_modules'))) {
    if (!run('npm', ['ci', '--no-audit', '--no-fund'], { cwd: join(work, 'client') }).ok) return false;
  }
  if (!run('node', ['client/scripts/bump-version.mjs', version], { cwd: work, quiet: true }).ok) return false;

  const env = {};
  if (existsSync(join(secrets, 'bebok.key'))) {
    env.TAURI_SIGNING_PRIVATE_KEY = readFileSync(join(secrets, 'bebok.key'), 'utf8').trim();
    env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD = process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? '';
  }
  if (!run('cargo', ['build', '--release', '--locked', '-p', 'bebok-server', '--target', triple], { cwd: join(work, 'engine') }).ok) return false;
  if (!run('npm', ['run', 'sidecar:copy', '--', '--target', triple], { cwd: join(work, 'client'), quiet: true }).ok) return false;
  const override = join(work, 'client', 'tauri-release-override.json');
  writeFileSync(override, JSON.stringify(env.TAURI_SIGNING_PRIVATE_KEY ? { bundle: { createUpdaterArtifacts: true } } : {}));
  if (!run('npx', ['tauri', 'build', '--ci', '--target', triple, '--bundles', 'app,dmg', '--config', 'tauri-release-override.json'], { cwd: join(work, 'client'), env }).ok) return false;

  const p = join(outDir, platform);
  mkdirSync(p, { recursive: true });
  const bundle = join(work, 'client', 'src-tauri', 'target', triple, 'release', 'bundle');
  cpSync(join(work, 'engine', 'target', triple, 'release', 'bebok-server'), join(p, `bebok-server-${version}-${platform}`));
  for (const f of readdirSync(join(bundle, 'dmg'))) if (f.endsWith('.dmg')) cpSync(join(bundle, 'dmg', f), join(p, f));
  const mac = join(bundle, 'macos');
  const tgz = readdirSync(mac).find((f) => f.endsWith('.app.tar.gz'));
  if (tgz) {
    cpSync(join(mac, tgz), join(p, `bebok_${version}_${arch}.app.tar.gz`));
    if (existsSync(join(mac, `${tgz}.sig`))) cpSync(join(mac, `${tgz}.sig`), join(p, `bebok_${version}_${arch}.app.tar.gz.sig`));
  } else {
    run('tar', ['-czf', join(p, `bebok_${version}_${arch}.app.tar.gz`), '-C', mac, 'bebok.app'], { quiet: true });
  }
  run('bash', ['-c', `cd "${p}" && shasum -a 256 -- * > SHA256SUMS-${platform}.txt`], { quiet: true });
  return true;
}

function mergeChecksums(outDir) {
  const lines = [];
  for (const platform of readdirSync(outDir)) {
    const dir = join(outDir, platform);
    for (const f of existsSync(dir) ? readdirSync(dir) : []) {
      if (/^SHA256SUMS-.*\.txt$/.test(f)) {
        for (const line of readFileSync(join(dir, f), 'utf8').split('\n')) if (line.trim()) lines.push(line.replace(/ {1,2}\*?/, `  ${platform}/`));
      }
    }
  }
  if (lines.length) writeFileSync(join(outDir, 'SHA256SUMS.txt'), `${lines.sort((a, b) => a.split(/\s+/)[1].localeCompare(b.split(/\s+/)[1])).join('\n')}\n`);
}

function listFiles(outDir) {
  const out = [];
  for (const entry of readdirSync(outDir, { withFileTypes: true })) {
    if (entry.name.startsWith('.')) continue;
    if (entry.isDirectory()) {
      for (const f of readdirSync(join(outDir, entry.name))) out.push(`${entry.name}/${f}`);
    } else {
      out.push(entry.name);
    }
  }
  return out.sort();
}

try {
  await main();
} finally {
  rl?.close();
}
