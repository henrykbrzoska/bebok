#!/usr/bin/env node
// builder.mjs — build release artifacts locally, the way CI does (zero deps, Node >= 20).
//
//   npm run builder                              interactive: pick targets + version
//   npm run builder -- --targets linux,android --version 1.8.0-test1 --yes
//   npm run builder -- --out ~/somewhere                 (default: ~/bebok-dist)
//   npm run builder:up | builder:down | builder:status   the Docker stack (idle containers; a build is an exec)
//
// What can be built depends on the host (checked up front, the menu says so):
//
//   linux    Docker (ubuntu:22.04 like CI)                     any host with Docker
//   android  Docker (JDK 21 + SDK + NDK like CI)               any host with Docker
//   windows  native on Windows (msi + nsis, exactly CI's leg)  Windows host
//            otherwise cross-built in Docker (nsis only, cargo-xwin, experimental)
//   macos    native on macOS (app + dmg, exactly CI's leg)      macOS host only - no container can build it
//
// Everything works on a private copy - the checkout keeps its version - and
// lands in <out>/<version>/<platform>/ with CI's file names, signed like CI
// when ~/.tauri holds the updater key / Android keystore.

import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
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

const hasDocker = () => run('docker', ['compose', 'version'], { quiet: true }).ok;
const DOCKER_TARGETS = ['linux', 'windows', 'android'];
const composeEnv = () => ({ BEBOK_SRC: ROOT, BEBOK_OUT: OUT, BEBOK_SECRETS: join(homedir(), '.tauri') });
const compose = (args, opts = {}) => run('docker', ['compose', '-f', COMPOSE, ...args], { env: composeEnv(), ...opts });

/** Running builder containers, by service name. */
function stackStatus() {
  const r = compose(['ps', '--format', '{{.Service}}\t{{.State}}'], { quiet: true });
  const up = new Set();
  for (const line of r.out.split('\n')) {
    const [service, state] = line.split('\t');
    if (state === 'running') up.add(service);
  }
  return up;
}

/** Start (and build the image of) the idle container for `service`; a no-op when it is already up. */
function stackUp(services, { build = false } = {}) {
  return compose(['up', '-d', ...(build ? ['--build'] : []), ...services]).ok;
}
const hasCargo = () => run('cargo', ['--version'], { quiet: true }).ok;

/**
 * How each target can be built on this host. `mode` is 'docker', 'native' or
 * null (not possible here); `why` explains a null / a downgrade.
 */
function hostPlan() {
  const docker = hasDocker();
  const cargo = hasCargo();
  const dockerOr = (why) => (docker ? { mode: 'docker' } : { mode: null, why: `needs Docker${why ? ` (${why})` : ''}` });
  return [
    { id: 'linux', label: 'Linux x64 (deb, AppImage, portable)', ...dockerOr() },
    { id: 'android', label: 'Android arm64 (APK)', ...dockerOr() },
    {
      id: 'windows',
      label: isWin
        ? 'Windows x64 (MSI + NSIS installers, portable) - native, same as CI'
        : 'Windows x64 (NSIS installer, portable) - cross-built in Docker, experimental; MSI needs a Windows host',
      ...(isWin ? (cargo ? { mode: 'native' } : { mode: null, why: 'needs cargo + node on this Windows host' }) : dockerOr('cross-build')),
    },
    {
      id: 'macos',
      label: `macOS ${process.arch === 'arm64' ? 'Apple silicon' : 'Intel'} (dmg, app.tar.gz) - native, same as CI`,
      ...(isMac ? (cargo ? { mode: 'native' } : { mode: null, why: 'needs cargo + Xcode command line tools' }) : { mode: null, why: 'only on a macOS host (no container can build .app/.dmg)' }),
    },
  ];
}

let rl = null;
async function ask(q, fallback) {
  if (yes) return fallback;
  rl ??= createInterface({ input: process.stdin, output: process.stdout });
  const a = (await rl.question(`${q}${fallback !== undefined ? c.dim(` [${fallback}]`) : ''} `)).trim();
  return a || fallback;
}

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { stdio: opts.quiet ? 'pipe' : 'inherit', cwd: opts.cwd ?? ROOT, env: { ...process.env, ...(opts.env ?? {}) }, encoding: 'utf8', windowsHide: true, shell: opts.shell ?? false });
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

  // ---- stack management: `--up` / `--down` / `--status` (the containers idle between builds)
  if (argv.includes('--up') || argv.includes('--down') || argv.includes('--status')) {
    if (!hasDocker()) fail('Docker (with compose) is required');
    if (argv.includes('--down')) {
      if (!compose(['down']).ok) fail('docker compose down failed');
    } else if (argv.includes('--up')) {
      if (!stackUp(DOCKER_TARGETS, { build: true })) fail('docker compose up failed');
    }
    const up = stackStatus();
    log(`${c.bold('Builder stack')} ${c.dim('(docker compose project bebok-builder)')}`);
    for (const id of DOCKER_TARGETS) log(`  ${up.has(id) ? c.green('●') : c.dim('○')} bebok-builder-${id}  ${up.has(id) ? 'running (idle)' : c.dim('stopped')}`);
    return;
  }

  // ---- targets, gated by what this host can do
  const TARGETS = hostPlan();
  const hostName = isMac ? 'macOS' : isWin ? 'Windows' : 'Linux';
  log(c.dim(`host: ${hostName} ${process.arch}`));
  let picked = (flag('targets') ?? '').split(',').map((s) => s.trim()).filter(Boolean);
  if (picked.length === 0) {
    TARGETS.forEach((t, i) => {
      const how = t.mode === 'docker' ? c.dim('docker') : t.mode === 'native' ? c.green('native') : c.red(`unavailable: ${t.why}`);
      log(`  ${t.mode ? i + 1 : '-'}) ${t.label}  ${how}`);
    });
    const answer = await ask('\nWhich targets? (numbers, comma-separated, or "all")', 'all');
    picked =
      answer === 'all'
        ? TARGETS.filter((t) => t.mode).map((t) => t.id)
        : answer.split(/[,\s]+/).map((n) => TARGETS[Number(n) - 1]?.id).filter(Boolean);
  }
  const targets = TARGETS.filter((t) => picked.includes(t.id));
  if (targets.length === 0) fail('no targets selected');
  const blocked = targets.filter((t) => !t.mode);
  if (blocked.length) fail(blocked.map((t) => `${t.id}: ${t.why}`).join('\n  '));

  // ---- version
  let version = flag('version') ?? (await ask('Version for these builds', suggestVersion()));
  version = version.replace(/^v/, '');
  if (!VERSION_RE.test(version)) fail(`'${version}' is not X.Y.Z[-suffix]`);
  const outDir = join(OUT, version);
  mkdirSync(outDir, { recursive: true });

  const secrets = join(homedir(), '.tauri');
  const hasUpdaterKey = existsSync(join(secrets, 'bebok.key'));
  const hasKeystore = existsSync(join(secrets, 'bebok-android.keystore'));
  log(`\n${c.bold('Plan')}`);
  log(`  version   ${version}`);
  log(`  targets   ${targets.map((t) => `${t.id} (${t.mode})`).join(', ')}`);
  log(`  updater   ${hasUpdaterKey ? c.green('signed (~/.tauri/bebok.key)') : c.yellow('unsigned - not installable as an update')}`);
  if (targets.some((t) => t.id === 'android')) log(`  apk       ${hasKeystore ? c.green('signed (~/.tauri/bebok-android.keystore)') : c.yellow('debug key')}`);
  log(`  output    ${outDir}\n`);
  if (!yes && (await ask('Go? (Y/n)', 'y')).toLowerCase().startsWith('n')) fail('aborted');

  const results = [];
  for (const t of targets) {
    const started = Date.now();
    log(`\n${c.bold(`== ${t.id} ==`)}`);
    let ok;
    if (t.mode === 'docker') {
      ok = (stackStatus().has(t.id) || stackUp([t.id], { build: true }))
        && compose(['exec', '-T', '-e', `VERSION=${version}`, t.id, 'bebok-build']).ok;
    } else {
      ok = buildNative(t.id, version, outDir, secrets);
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

/**
 * A native leg (macOS on a Mac, Windows on Windows): the same steps as the
 * matching release.yml matrix entry, on a private copy of the checkout.
 */
function buildNative(id, version, outDir, secrets) {
  const mac = id === 'macos';
  const triple = mac ? (process.arch === 'arm64' ? 'aarch64-apple-darwin' : 'x86_64-apple-darwin') : 'x86_64-pc-windows-msvc';
  const platform = mac ? (process.arch === 'arm64' ? 'macos-arm64' : 'macos-x64') : 'windows-x64';
  const arch = process.arch === 'arm64' ? 'aarch64' : 'x64';
  const exe = mac ? '' : '.exe';
  const work = join(OUT, `.work-${id}`);
  log(c.dim(`private copy -> ${work}`));
  rmSync(work, { recursive: true, force: true });
  mkdirSync(work, { recursive: true });
  // Tracked + untracked-not-ignored files, like the container's rsync.
  const files = run('git', ['ls-files', '-z', '--cached', '--others', '--exclude-standard'], { quiet: true }).out.split('\0').filter(Boolean);
  for (const f of files) {
    if (/^(client\/node_modules|relay\/node_modules|engine\/target|client\/src-tauri\/target)\//.test(f)) continue;
    const src = join(ROOT, f);
    if (!existsSync(src) || statSync(src).isDirectory()) continue;
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
      try {
        symlinkSync(join(ROOT, from), join(work, to), isWin ? 'junction' : 'dir');
      } catch {
        /* falls back to npm ci / a cold cargo build below */
      }
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
  if (!run(isWin ? 'npm.cmd' : 'npm', ['run', 'sidecar:copy', '--', '--target', triple], { cwd: join(work, 'client'), quiet: true, shell: isWin }).ok) return false;
  const override = join(work, 'client', 'tauri-release-override.json');
  writeFileSync(override, JSON.stringify(env.TAURI_SIGNING_PRIVATE_KEY ? { bundle: { createUpdaterArtifacts: true } } : {}));
  const bundles = mac ? 'app,dmg' : 'msi,nsis';
  if (!run(isWin ? 'npx.cmd' : 'npx', ['tauri', 'build', '--ci', '--target', triple, '--bundles', bundles, '--config', 'tauri-release-override.json'], { cwd: join(work, 'client'), env, shell: isWin }).ok) return false;

  const p = join(outDir, platform);
  mkdirSync(p, { recursive: true });
  const bundle = join(work, 'client', 'src-tauri', 'target', triple, 'release', 'bundle');
  const bin = join(work, 'client', 'src-tauri', 'target', triple, 'release');
  cpSync(join(work, 'engine', 'target', triple, 'release', `bebok-server${exe}`), join(p, `bebok-server-${version}-${platform}${exe}`));
  if (mac) {
    for (const f of readdirSync(join(bundle, 'dmg'))) if (f.endsWith('.dmg')) cpSync(join(bundle, 'dmg', f), join(p, f));
    const macDir = join(bundle, 'macos');
    const tgz = readdirSync(macDir).find((f) => f.endsWith('.app.tar.gz'));
    if (tgz) {
      cpSync(join(macDir, tgz), join(p, `bebok_${version}_${arch}.app.tar.gz`));
      if (existsSync(join(macDir, `${tgz}.sig`))) cpSync(join(macDir, `${tgz}.sig`), join(p, `bebok_${version}_${arch}.app.tar.gz.sig`));
    } else {
      run('tar', ['-czf', join(p, `bebok_${version}_${arch}.app.tar.gz`), '-C', macDir, 'bebok.app'], { quiet: true });
    }
    run('bash', ['-c', `cd "${p}" && shasum -a 256 -- * > SHA256SUMS-${platform}.txt`], { quiet: true });
  } else {
    for (const sub of ['msi', 'nsis']) {
      const dir = join(bundle, sub);
      if (!existsSync(dir)) continue;
      for (const f of readdirSync(dir)) if (/\.(msi|exe)(\.sig)?$/.test(f)) cpSync(join(dir, f), join(p, f));
    }
    // Portable zip like CI: desktop exe + sidecar + LICENSE + README.
    const stage = join(OUT, '.stage', `bebok-${version}`);
    rmSync(join(OUT, '.stage'), { recursive: true, force: true });
    mkdirSync(stage, { recursive: true });
    const app = existsSync(join(bin, 'bebok-desktop.exe')) ? join(bin, 'bebok-desktop.exe') : join(bin, 'bebok.exe');
    for (const [from, to] of [[app, 'bebok-desktop.exe'], [join(work, 'engine', 'target', triple, 'release', 'bebok-server.exe'), 'bebok-server.exe'], [join(work, 'LICENSE'), 'LICENSE'], [join(work, 'README.md'), 'README.md']]) cpSync(from, join(stage, to));
    run('powershell', ['-NoProfile', '-Command', `Compress-Archive -Force -Path '${stage}' -DestinationPath '${join(p, `bebok-${version}-${platform}-portable.zip`)}'`], { quiet: true });
    run('powershell', ['-NoProfile', '-Command', `cd '${p}'; Get-ChildItem -File | Where-Object { $_.Name -ne 'SHA256SUMS-${platform}.txt' } | ForEach-Object { "$((Get-FileHash $_.FullName -Algorithm SHA256).Hash.ToLower())  $($_.Name)" } | Set-Content -Encoding ascii 'SHA256SUMS-${platform}.txt'`], { quiet: true });
  }
  return true;
}

function mergeChecksums(outDir) {
  const lines = [];
  for (const platform of readdirSync(outDir)) {
    const dir = join(outDir, platform);
    if (!statSync(dir).isDirectory()) continue;
    for (const f of readdirSync(dir)) {
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
