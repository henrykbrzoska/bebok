#!/usr/bin/env node
// bebok.mjs - cross-platform build/run orchestration for the Bebok repo (F9-17).
//
//   npm run doctor                       toolchain + platform dependency check
//   npm run full-build-dev [-- flags]    engine (dev) + `ng serve`, token wired automatically
//   npm run full-build-app [-- flags]    engine (release) + sidecar + Tauri bundles for this OS
//   npm run full-build-apk [-- flags]    engine (per-ABI) + ng build + cap sync + Android APK
//   npm run engine         [-- flags]    build + run the engine only
//   npm run client         [-- flags]    `ng serve` only
//
// Plain Node >= 20 (22 recommended), no dependencies: child processes are
// spawned with `shell: false` and platform-aware entry points (`node.exe`
// + the JS entry of npm / ng / tauri instead of the `.cmd` shims, `cargo`
// / `rustc` as real executables). Works on Windows, Linux and macOS.
//
// Run `node scripts/bebok.mjs --help` for the flag reference.

import { spawn, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { createReadStream, existsSync, readdirSync, statSync } from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

// ---------------------------------------------------------------------------
// Paths / platform
// ---------------------------------------------------------------------------

const ROOT = path.dirname(path.dirname(fileURLToPath(import.meta.url)));
const ENGINE_DIR = path.join(ROOT, 'engine');
const CLIENT_DIR = path.join(ROOT, 'client');
const TAURI_DIR = path.join(CLIENT_DIR, 'src-tauri');

const isWin = process.platform === 'win32';
const isMac = process.platform === 'darwin';
const isLinux = process.platform === 'linux';
const EXE = isWin ? '.exe' : '';

const DEFAULT_ENGINE_PORT = 8787;
const DEFAULT_CLIENT_PORT = 4200;
const READY_TIMEOUT_MS = 90_000;
const CLIENT_TIMEOUT_MS = 240_000;

// ---------------------------------------------------------------------------
// Output helpers (colour only on a TTY, honours NO_COLOR)
// ---------------------------------------------------------------------------

const useColor = process.stdout.isTTY && !process.env.NO_COLOR && process.env.TERM !== 'dumb';
const paint = (code) => (s) => (useColor ? `\u001b[${code}m${s}\u001b[0m` : s);
const c = {
  cyan: paint('36'),
  magenta: paint('35'),
  green: paint('32'),
  yellow: paint('33'),
  red: paint('31'),
  dim: paint('2'),
  bold: paint('1'),
};

const TAGS = {
  engine: c.cyan('[engine]'),
  client: c.magenta('[client]'),
  tauri: c.magenta('[tauri] '),
  bebok: c.green('[bebok] '),
  cargo: c.cyan('[cargo] '),
  npm: c.magenta('[npm]   '),
  android: c.cyan('[apk]   '),
};

function log(msg) {
  process.stdout.write(`${TAGS.bebok} ${msg}\n`);
}
function warn(msg) {
  process.stderr.write(`${TAGS.bebok} ${c.yellow('warning:')} ${msg}\n`);
}
function fail(msg, code = 1) {
  process.stderr.write(`${TAGS.bebok} ${c.red('error:')} ${msg}\n`);
  process.exit(code);
}

// ---------------------------------------------------------------------------
// Executable resolution (no shell, no .cmd shims)
// ---------------------------------------------------------------------------

/** JS entry of the npm that runs us (or the one next to node). */
function npmCli() {
  const nodeDir = path.dirname(process.execPath);
  const candidates = [
    process.env.npm_execpath,
    path.join(nodeDir, 'node_modules', 'npm', 'bin', 'npm-cli.js'),
    path.join(nodeDir, '..', 'lib', 'node_modules', 'npm', 'bin', 'npm-cli.js'),
  ].filter(Boolean);
  return candidates.find((p) => existsSync(p)) ?? null;
}

const NG_JS = path.join(CLIENT_DIR, 'node_modules', '@angular', 'cli', 'bin', 'ng.js');
const TAURI_JS = path.join(CLIENT_DIR, 'node_modules', '@tauri-apps', 'cli', 'tauri.js');
const COPY_SIDECAR_JS = path.join(CLIENT_DIR, 'scripts', 'copy-sidecar.mjs');
const CAP_JS = path.join(CLIENT_DIR, 'node_modules', '@capacitor', 'cli', 'bin', 'capacitor');
const ANDROID_DIR = path.join(CLIENT_DIR, 'android');
const BUNDLE_ANDROID_SH = path.join(CLIENT_DIR, 'scripts', 'bundle-android.sh');

/** `cargo` / `rustc` are real executables; `where`/`which` them via PATH. */
function findOnPath(name) {
  const exts = isWin ? (process.env.PATHEXT || '.EXE;.CMD;.BAT').split(';') : [''];
  for (const dir of (process.env.PATH || '').split(path.delimiter)) {
    if (!dir) continue;
    for (const ext of exts) {
      const p = path.join(dir, name + ext.toLowerCase());
      if (existsSync(p)) return p;
      const P = path.join(dir, name + ext);
      if (existsSync(P)) return P;
    }
  }
  return null;
}

/**
 * `bundle-android.sh` is a bash script (POSIX sh, runs on Ubuntu CI and on
 * Windows via Git Bash/WSL - see the script header). Resolve a real `bash`
 * rather than relying on a shell to interpret the `.sh` extension.
 */
function bashBin() {
  const onPath = findOnPath('bash');
  if (onPath) return onPath;
  if (isWin) {
    // Git for Windows' default install locations (Git Bash), tried when
    // `bash` is not already on PATH.
    const candidates = [
      'C:\\Program Files\\Git\\bin\\bash.exe',
      'C:\\Program Files (x86)\\Git\\bin\\bash.exe',
    ];
    return candidates.find((p) => existsSync(p)) ?? null;
  }
  return null;
}

function cargoBin(name) {
  const home = path.join(os.homedir(), '.cargo', 'bin', name + EXE);
  if (existsSync(home)) return home;
  return findOnPath(name) ?? name;
}

// ---------------------------------------------------------------------------
// Process helpers
// ---------------------------------------------------------------------------

const children = new Set();
let shuttingDown = false;

/**
 * Spawn `cmd args` with merged, prefixed output. POSIX children get their own
 * process group (`detached`) so the whole tree can be signalled at once;
 * Windows trees are killed with `taskkill /T`.
 */
function spawnTagged(tag, cmd, args, { cwd = ROOT, env = {}, onStdoutLine } = {}) {
  const child = spawn(cmd, args, {
    cwd,
    env: { ...process.env, ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
    shell: false,
    detached: !isWin,
    windowsHide: true,
  });
  children.add(child);
  child.on('exit', () => children.delete(child));

  const pipe = (stream, isErr) => {
    const rl = createInterface({ input: stream, crlfDelay: Infinity });
    rl.on('line', (line) => {
      if (onStdoutLine && !isErr) onStdoutLine(line);
      (isErr ? process.stderr : process.stdout).write(`${tag} ${line}\n`);
    });
  };
  pipe(child.stdout, false);
  pipe(child.stderr, true);
  return child;
}

/** Run to completion, fail the script on a non-zero exit. */
function runStep(tag, cmd, args, opts = {}) {
  const shown = [path.basename(cmd), ...args].join(' ');
  log(`${c.dim('$')} ${shown}${opts.cwd ? c.dim(`  (in ${path.relative(ROOT, opts.cwd) || '.'})`) : ''}`);
  return new Promise((resolve) => {
    const child = spawnTagged(tag, cmd, args, opts);
    child.on('error', (err) => fail(`${shown}: ${err.message}`));
    child.on('exit', (code, signal) => {
      if (shuttingDown) return resolve();
      if (code !== 0) fail(`${shown} exited with ${signal ?? code}`, code || 1);
      resolve();
    });
  });
}

function killTree(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (isWin) {
    spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
    return;
  }
  try {
    process.kill(-child.pid, 'SIGTERM');
  } catch {
    try {
      child.kill('SIGTERM');
    } catch {
      /* already gone */
    }
  }
  setTimeout(() => {
    try {
      process.kill(-child.pid, 'SIGKILL');
    } catch {
      /* already gone */
    }
  }, 3000).unref();
}

function shutdown(code = 0, why = '') {
  if (shuttingDown) return;
  shuttingDown = true;
  if (why) log(why);
  process.exitCode = code;
  for (const child of [...children]) killTree(child);
  // Give taskkill / SIGTERM a moment to propagate before we exit (ref'd on
  // purpose: once the children are gone the loop would otherwise drain and
  // exit before this fires - exitCode above covers that path too).
  setTimeout(() => process.exit(code), isWin ? 800 : 400);
}

for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP', 'SIGBREAK']) {
  try {
    process.on(sig, () => shutdown(130, `${sig} received - stopping children`));
  } catch {
    /* signal unsupported on this platform */
  }
}
process.on('exit', () => {
  for (const child of [...children]) killTree(child);
});

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

/** Resolve once `url` answers any HTTP status (the dev server is up). */
async function waitForHttp(url, timeoutMs, label) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    if (shuttingDown) return false;
    const ok = await new Promise((resolve) => {
      const req = http.get(url, (res) => {
        res.resume();
        resolve(true);
      });
      req.on('error', () => resolve(false));
      req.setTimeout(2000, () => {
        req.destroy();
        resolve(false);
      });
    });
    if (ok) return true;
    await sleep(500);
  }
  fail(`${label} did not answer at ${url} within ${Math.round(timeoutMs / 1000)}s`);
}

function openBrowser(url) {
  let cmd;
  let args;
  if (isWin) {
    // No shell: `start` would need cmd.exe quoting; rundll32 takes the URL verbatim.
    cmd = 'rundll32';
    args = ['url.dll,FileProtocolHandler', url];
  } else if (isMac) {
    cmd = 'open';
    args = [url];
  } else {
    cmd = 'xdg-open';
    args = [url];
  }
  try {
    const child = spawn(cmd, args, { stdio: 'ignore', detached: true, shell: false, windowsHide: true });
    child.on('error', (err) => warn(`could not open a browser (${err.message}); open ${url} yourself`));
    child.unref();
  } catch (err) {
    warn(`could not open a browser (${err.message}); open ${url} yourself`);
  }
}

// ---------------------------------------------------------------------------
// Toolchain probes
// ---------------------------------------------------------------------------

function probe(cmd, args, { cwd } = {}) {
  try {
    const r = spawnSync(cmd, args, { encoding: 'utf8', cwd, windowsHide: true, shell: false });
    if (r.error || r.status !== 0) return null;
    return `${r.stdout || ''}${r.stderr || ''}`.trim();
  } catch {
    return null;
  }
}

function hostTriple() {
  const out = probe(cargoBin('rustc'), ['-vV']);
  const m = out && out.match(/host: (\S+)/);
  if (m) return m[1];
  const arch = process.arch === 'x64' ? 'x86_64' : process.arch === 'arm64' ? 'aarch64' : process.arch;
  if (isWin) return `${arch}-pc-windows-msvc`;
  if (isMac) return `${arch}-apple-darwin`;
  return `${arch}-unknown-linux-gnu`;
}

function nodeVersion() {
  return process.versions.node;
}

function hasClientDeps() {
  return existsSync(NG_JS);
}

async function ensureClientDeps() {
  if (hasClientDeps()) return;
  log('client/node_modules missing - installing client dependencies');
  await runNpm(['ci', '--no-audit', '--no-fund'], { cwd: CLIENT_DIR, tag: TAGS.npm });
  if (!hasClientDeps()) fail('client dependencies are still missing after npm ci');
}

async function runNpm(args, { cwd = CLIENT_DIR, tag = TAGS.npm } = {}) {
  const cli = npmCli();
  if (cli) return runStep(tag, process.execPath, [cli, ...args], { cwd });
  // Last resort: npm's own shim. On Windows that is npm.cmd, which Node refuses
  // to spawn without a shell (CVE-2024-27980), so this one call uses one.
  if (isWin) {
    return runStep(tag, 'cmd.exe', ['/d', '/s', '/c', `npm ${args.join(' ')}`], { cwd });
  }
  return runStep(tag, 'npm', args, { cwd });
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

function engineBinary(profile, triple) {
  const dir = triple
    ? path.join(ENGINE_DIR, 'target', triple, profile)
    : path.join(ENGINE_DIR, 'target', profile);
  return path.join(dir, `bebok-server${EXE}`);
}

async function buildEngine({ release = false, triple = null } = {}) {
  const args = ['build', '-p', 'bebok-server'];
  if (release) args.push('--release');
  if (triple) args.push('--target', triple);
  await runStep(TAGS.cargo, cargoBin('cargo'), args, { cwd: ENGINE_DIR });
  const bin = engineBinary(release ? 'release' : 'debug', triple);
  if (!existsSync(bin)) fail(`engine binary not found after build: ${bin}`);
  return bin;
}

function engineEnv(flags, clientPort) {
  const env = {
    // The engine only allows localhost:4200 by default; a custom client port
    // (and 127.0.0.1) must be whitelisted or the browser blocks every call.
    BEBOK_CORS: [
      process.env.BEBOK_CORS,
      `http://localhost:${clientPort}`,
      `http://127.0.0.1:${clientPort}`,
    ]
      .filter(Boolean)
      .join(','),
  };
  if (flags['no-auth']) env.BEBOK_NO_AUTH = '1';
  if (flags.diagnostic) env.BEBOK_DIAGNOSTIC = '1';
  return env;
}

/** Start the engine and resolve with its `BEBOK_READY` URL (token included). */
function startEngine(bin, port, env) {
  return new Promise((resolve) => {
    let ready = false;
    const timer = setTimeout(() => {
      if (!ready) fail(`engine did not print BEBOK_READY within ${READY_TIMEOUT_MS / 1000}s`);
    }, READY_TIMEOUT_MS);
    const child = spawnTagged(TAGS.engine, bin, ['--port', String(port)], {
      cwd: ENGINE_DIR,
      env,
      onStdoutLine: (line) => {
        const m = line.match(/^BEBOK_READY\s+(\S+)/);
        if (m && !ready) {
          ready = true;
          clearTimeout(timer);
          resolve({ child, url: m[1] });
        }
      },
    });
    child.on('error', (err) => fail(`engine failed to start: ${err.message}`));
    child.on('exit', (code, signal) => {
      if (shuttingDown) return;
      if (!ready) fail(`engine exited before BEBOK_READY (${signal ?? code})`, code || 1);
      shutdown(code ?? 1, `engine exited (${signal ?? code}) - stopping`);
    });
  });
}

function startNgServe(port) {
  const child = spawnTagged(
    TAGS.client,
    process.execPath,
    [NG_JS, 'serve', '--port', String(port), '--host', 'localhost'],
    { cwd: CLIENT_DIR, env: { NG_CLI_ANALYTICS: 'false' } },
  );
  child.on('error', (err) => fail(`ng serve failed to start: ${err.message}`));
  child.on('exit', (code, signal) => {
    if (shuttingDown) return;
    shutdown(code ?? 1, `ng serve exited (${signal ?? code}) - stopping`);
  });
  return child;
}

function bootstrapUrl(clientPort, engineUrl) {
  return `http://localhost:${clientPort}/?engine=${encodeURIComponent(engineUrl)}`;
}

async function cmdFullBuildDev(flags) {
  const port = Number(flags.port ?? DEFAULT_ENGINE_PORT);
  const clientPort = Number(flags['client-port'] ?? DEFAULT_CLIENT_PORT);
  await ensureClientDeps();

  if (flags.tauri) {
    // Desktop dev: the Tauri shell spawns `binaries/bebok-server-<triple>` as
    // its own sidecar (random port, token passed via the `engine_info`
    // command), so no separate engine process is started here. `tauri dev`
    // runs `npm run start` itself (beforeDevCommand) against the fixed
    // devUrl http://localhost:4200 - --client-port/--port do not apply.
    if (flags['client-port'] || flags.port) {
      warn('--tauri ignores --port/--client-port (tauri.conf.json devUrl is fixed to :4200)');
    }
    const triple = hostTriple();
    log(`desktop dev: engine release build (${triple}) -> sidecar -> tauri dev`);
    await buildEngine({ release: true });
    await runStep(TAGS.npm, process.execPath, [COPY_SIDECAR_JS], { cwd: CLIENT_DIR });
    const env = engineEnv(flags, DEFAULT_CLIENT_PORT);
    log(`${c.dim('$')} tauri dev  ${c.dim('(Ctrl+C stops the shell, ng serve and the sidecar)')}`);
    const child = spawnTagged(TAGS.tauri, process.execPath, [TAURI_JS, 'dev'], { cwd: CLIENT_DIR, env });
    child.on('exit', (code, signal) => shutdown(code ?? 0, `tauri dev exited (${signal ?? code})`));
    return;
  }

  const bin = await buildEngine({ release: false });
  const env = engineEnv(flags, clientPort);
  log(`starting engine on :${port}${flags['no-auth'] ? ' (BEBOK_NO_AUTH=1)' : ''}${flags.diagnostic ? ' (BEBOK_DIAGNOSTIC=1)' : ''}`);
  const { url: engineUrl } = await startEngine(bin, port, env);
  log(`engine ready: ${c.bold(engineUrl)}`);

  log(`starting ng serve on :${clientPort}`);
  startNgServe(clientPort);
  await waitForHttp(`http://localhost:${clientPort}/`, CLIENT_TIMEOUT_MS, 'ng serve');

  // The client adopts `?engine=` on first load (transport.strategy.ts): it
  // stores the engine URL + token exactly like a manual Connect and strips
  // the parameter from the address bar. No paste needed.
  const url = bootstrapUrl(clientPort, engineUrl);
  log('');
  log(`${c.green('ready')}  open ${c.bold(url)}`);
  log(c.dim('       (the token is adopted once and removed from the address bar; Ctrl+C stops both)'));
  log('');
  if (flags.open) openBrowser(url);
}

async function cmdEngine(flags) {
  const port = Number(flags.port ?? DEFAULT_ENGINE_PORT);
  const clientPort = Number(flags['client-port'] ?? DEFAULT_CLIENT_PORT);
  const bin = await buildEngine({ release: !!flags.release });
  const { url } = await startEngine(bin, port, engineEnv(flags, clientPort));
  log(`engine ready: ${c.bold(url)}`);
  log(`browser client bootstrap: ${bootstrapUrl(clientPort, url)}`);
}

async function cmdClient(flags) {
  const clientPort = Number(flags['client-port'] ?? DEFAULT_CLIENT_PORT);
  await ensureClientDeps();
  startNgServe(clientPort);
  await waitForHttp(`http://localhost:${clientPort}/`, CLIENT_TIMEOUT_MS, 'ng serve');
  const url = flags.engine
    ? bootstrapUrl(clientPort, flags.engine)
    : `http://localhost:${clientPort}/`;
  log(`${c.green('ready')}  ${url}`);
  if (flags.open) openBrowser(url);
}

function defaultBundles() {
  if (isWin) return 'nsis,msi';
  if (isMac) return 'dmg';
  return 'deb,appimage';
}

function sha256(file) {
  return new Promise((resolve, reject) => {
    const h = createHash('sha256');
    createReadStream(file).on('data', (d) => h.update(d)).on('end', () => resolve(h.digest('hex'))).on('error', reject);
  });
}

function listFiles(dir, depth = 2) {
  if (!existsSync(dir)) return [];
  const out = [];
  for (const name of readdirSync(dir)) {
    const p = path.join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) {
      if (depth > 0 && !name.endsWith('.app')) out.push(...listFiles(p, depth - 1));
      else if (name.endsWith('.app')) out.push(p);
    } else out.push(p);
  }
  return out;
}

async function cmdFullBuildApp(flags) {
  const triple = hostTriple();
  const bundles = flags.bundles || defaultBundles();
  await ensureClientDeps();

  let engineBin = engineBinary('release', triple);
  if (flags['skip-engine']) {
    if (!existsSync(engineBin)) {
      engineBin = engineBinary('release');
    }
    log(`--skip-engine: using ${engineBin}`);
    if (!existsSync(engineBin)) fail('no release engine binary found; drop --skip-engine');
  } else {
    engineBin = await buildEngine({ release: true, triple });
  }

  if (flags['skip-tauri']) {
    await runStep(TAGS.client, process.execPath, [NG_JS, 'build'], {
      cwd: CLIENT_DIR,
      env: { NG_CLI_ANALYTICS: 'false' },
    });
  } else {
    await runStep(TAGS.npm, process.execPath, [COPY_SIDECAR_JS, '--target', triple], { cwd: CLIENT_DIR });
    // `tauri build` runs the production `ng build` itself (beforeBuildCommand).
    await runStep(
      TAGS.tauri,
      process.execPath,
      [TAURI_JS, 'build', '--ci', '--target', triple, '--bundles', bundles],
      { cwd: CLIENT_DIR, env: { NG_CLI_ANALYTICS: 'false' } },
    );
  }

  // ---- report ---------------------------------------------------------------
  const artifacts = [engineBin];
  if (!flags['skip-tauri']) {
    const rel = path.join(TAURI_DIR, 'target', triple, 'release');
    for (const n of [`bebok-desktop${EXE}`, `bebok${EXE}`]) {
      if (existsSync(path.join(rel, n))) artifacts.push(path.join(rel, n));
    }
    artifacts.push(...listFiles(path.join(rel, 'bundle'), 2));
  } else {
    const dist = path.join(CLIENT_DIR, 'dist', 'bebok', 'browser');
    if (existsSync(dist)) artifacts.push(dist);
  }

  log('');
  log(`${c.green('build complete')} (${triple}${flags['skip-tauri'] ? ', tauri skipped' : `, bundles: ${bundles}`})`);
  for (const p of artifacts) {
    const st = statSync(p);
    if (st.isDirectory()) {
      log(`  ${path.relative(ROOT, p)}  ${c.dim('(directory)')}`);
      continue;
    }
    const mb = (st.size / 1024 / 1024).toFixed(1);
    log(`  ${path.relative(ROOT, p)}  ${c.dim(`${mb} MB`)}`);
    log(`    sha256 ${await sha256(p)}`);
  }
}

/**
 * WP-M3 (F10-11): Android APK, mirroring the android.yml/release.yml CI
 * pipeline for a local run - bundle the engine (per-ABI), `ng build`,
 * `cap sync android`, `gradlew assembleRelease`.
 */
async function cmdFullBuildApk(flags) {
  if (!existsSync(ANDROID_DIR)) {
    fail(`${path.relative(ROOT, ANDROID_DIR)} not found - this checkout has no Android project`);
  }
  const bash = bashBin();
  if (!bash) {
    fail(
      'no bash found - bundle-android.sh is a POSIX shell script; install Git for Windows ' +
        '(Git Bash) or run this from WSL/Linux/macOS',
    );
  }
  await ensureClientDeps();

  const abis = flags.abis || 'arm64-v8a';
  log(`bundling engine for ABIs: ${abis}`);
  await runStep(TAGS.android, bash, [BUNDLE_ANDROID_SH], {
    cwd: CLIENT_DIR,
    env: { BEBOK_ANDROID_ABIS: abis },
  });

  await runStep(TAGS.client, process.execPath, [NG_JS, 'build'], {
    cwd: CLIENT_DIR,
    env: { NG_CLI_ANALYTICS: 'false' },
  });

  await runStep(TAGS.android, process.execPath, [CAP_JS, 'sync', 'android'], { cwd: CLIENT_DIR });

  const gradlew = path.join(ANDROID_DIR, isWin ? 'gradlew.bat' : 'gradlew');
  if (!existsSync(gradlew)) fail(`gradle wrapper not found: ${gradlew}`);
  const gradleArgs = ['assembleRelease'];
  if (flags.test) gradleArgs.push('testDebugUnitTest');
  await runStep(TAGS.android, gradlew, gradleArgs, { cwd: ANDROID_DIR });

  // ---- report ---------------------------------------------------------------
  const apkDir = path.join(ANDROID_DIR, 'app', 'build', 'outputs', 'apk', 'release');
  const apks = existsSync(apkDir) ? listFiles(apkDir, 1).filter((p) => p.endsWith('.apk')) : [];
  if (!apks.length) fail(`no APK found under ${path.relative(ROOT, apkDir)}`);

  log('');
  log(`${c.green('build complete')} (ABIs: ${abis})`);
  for (const p of apks) {
    const st = statSync(p);
    const mb = (st.size / 1024 / 1024).toFixed(1);
    log(`  ${path.relative(ROOT, p)}  ${c.dim(`${mb} MB`)}`);
    log(`    sha256 ${await sha256(p)}`);
  }
}

async function cmdDoctor() {
  const rows = [];
  const add = (name, ok, detail, required = true) => rows.push({ name, ok, detail, required });

  const node = nodeVersion();
  add('node', Number(node.split('.')[0]) >= 20, `v${node}${Number(node.split('.')[0]) < 20 ? ' (need >= 20)' : ''}`);
  const npm = npmCli();
  add('npm', !!npm, npm ? probe(process.execPath, [npm, '--version']) ?? npm : 'npm-cli.js not found');

  const rustc = probe(cargoBin('rustc'), ['--version']);
  add('rustc', !!rustc, rustc ?? 'not found (https://rustup.rs)');
  const cargo = probe(cargoBin('cargo'), ['--version']);
  add('cargo', !!cargo, cargo ?? 'not found (https://rustup.rs)');
  add('host target', !!rustc, hostTriple(), false);

  add('client deps', hasClientDeps(), hasClientDeps() ? 'client/node_modules present' : 'run `npm ci` in client/ (full-build-* does it)', false);
  const tauriVer = existsSync(TAURI_JS) ? probe(process.execPath, [TAURI_JS, '--version']) : null;
  add('tauri cli', !!tauriVer, tauriVer ?? 'client/node_modules/@tauri-apps/cli missing (npm ci in client/)', false);

  if (isWin) {
    const keys = [
      'HKLM\\SOFTWARE\\WOW6432Node\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
      'HKLM\\SOFTWARE\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
      'HKCU\\Software\\Microsoft\\EdgeUpdate\\Clients\\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
    ];
    let wv2 = null;
    for (const k of keys) {
      const out = probe('reg', ['query', k, '/v', 'pv']);
      const m = out && out.match(/pv\s+REG_SZ\s+(\S+)/);
      if (m) {
        wv2 = m[1];
        break;
      }
    }
    add('WebView2 runtime', !!wv2, wv2 ?? 'not installed (https://developer.microsoft.com/microsoft-edge/webview2/)');
    const vswhere = path.join(process.env['ProgramFiles(x86)'] || 'C:\\Program Files (x86)', 'Microsoft Visual Studio', 'Installer', 'vswhere.exe');
    const vs = existsSync(vswhere)
      ? probe(vswhere, ['-latest', '-products', '*', '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationName'])
      : null;
    add('MSVC build tools', !!vs, vs || 'Visual Studio C++ build tools not found (needed by the msvc Rust target)');
  } else if (isLinux) {
    const pkgconfig = probe('pkg-config', ['--version']);
    add('pkg-config', !!pkgconfig, pkgconfig ?? 'not found');
    const libs = ['webkit2gtk-4.1', 'javascriptcoregtk-4.1', 'libsoup-3.0', 'gtk+-3.0', 'librsvg-2.0', 'openssl'];
    for (const lib of libs) {
      const v = pkgconfig ? probe('pkg-config', ['--modversion', lib]) : null;
      add(lib, !!v, v ?? `missing (apt: libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libssl-dev ...)`);
    }
    const ayatana = pkgconfig ? probe('pkg-config', ['--modversion', 'ayatana-appindicator3-0.1']) : null;
    add('appindicator', !!ayatana, ayatana ?? 'libayatana-appindicator3-dev missing (optional: tray icons)', false);
    add('cc', !!probe('cc', ['--version']), probe('cc', ['--version'])?.split('\n')[0] ?? 'no C compiler (build-essential)');
  } else if (isMac) {
    const clt = probe('xcode-select', ['-p']);
    add('Xcode CLT', !!clt, clt ?? 'run `xcode-select --install`');
  }

  log(`doctor (${process.platform} ${process.arch})`);
  let missing = 0;
  for (const r of rows) {
    const mark = r.ok ? c.green('ok     ') : r.required ? c.red('MISSING') : c.yellow('warn   ');
    if (!r.ok && r.required) missing++;
    log(`  ${mark} ${r.name.padEnd(18)} ${c.dim(String(r.detail).split('\n')[0])}`);
  }
  log(missing ? c.red(`${missing} required item(s) missing`) : c.green('all required tools present'));
  process.exitCode = missing ? 1 : 0;
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

const USAGE = `usage: node scripts/bebok.mjs <command> [flags]

commands
  doctor           check rustc/cargo, node/npm, tauri cli and OS-level Tauri deps
  full-build-dev   build engine (dev) + start it + ng serve; opens with the token wired
  full-build-app   engine release + sidecar + Tauri bundles for this OS (+ SHA256 report)
  full-build-apk   engine (per-ABI, Android) + ng build + cap sync + Android APK
  engine           build + run the engine only (prints the bootstrap URL)
  client           ng serve only

flags (full-build-dev / engine / client)
  --port <n>          engine port            (default ${DEFAULT_ENGINE_PORT})
  --client-port <n>   ng serve port          (default ${DEFAULT_CLIENT_PORT})
  --no-auth           BEBOK_NO_AUTH=1  (unauthenticated local API)
  --diagnostic        BEBOK_DIAGNOSTIC=1
  --open              open the default browser on the bootstrap URL
  --tauri             full-build-dev: run \`tauri dev\` (desktop shell + its own sidecar)
  --release           engine: build the release profile
  --engine <url>      client: bootstrap URL to open with (--open)

flags (full-build-app)
  --bundles <list>    tauri bundle list      (default: windows nsis,msi / linux deb,appimage / macos dmg)
  --skip-engine       reuse engine/target/<triple>/release/bebok-server
  --skip-tauri        stop after the engine release build + \`ng build\`

flags (full-build-apk)
  --abis <list>       space-separated Android ABIs -> BEBOK_ANDROID_ABIS (default: arm64-v8a)
  --test              also run \`gradlew testDebugUnitTest\`
  requires: bash (Git Bash/WSL on Windows), ANDROID_NDK_HOME (NDK r27), rustup
  targets aarch64-linux-android/x86_64-linux-android, Android SDK (ANDROID_HOME)
`;

async function main() {
  let parsed;
  try {
    parsed = parseArgs({
      args: process.argv.slice(2),
      allowPositionals: true,
      strict: true,
      options: {
        port: { type: 'string' },
        'client-port': { type: 'string' },
        'no-auth': { type: 'boolean', default: false },
        diagnostic: { type: 'boolean', default: false },
        open: { type: 'boolean', default: false },
        tauri: { type: 'boolean', default: false },
        release: { type: 'boolean', default: false },
        engine: { type: 'string' },
        bundles: { type: 'string' },
        'skip-engine': { type: 'boolean', default: false },
        'skip-tauri': { type: 'boolean', default: false },
        abis: { type: 'string' },
        test: { type: 'boolean', default: false },
        help: { type: 'boolean', short: 'h', default: false },
      },
    });
  } catch (err) {
    process.stderr.write(`${err.message}\n\n${USAGE}`);
    process.exit(2);
  }
  const { values: flags, positionals } = parsed;
  const command = positionals[0];
  // `dev.cmd tauri` compatibility: a bare "tauri" positional means --tauri.
  if (positionals.includes('tauri')) flags.tauri = true;

  if (flags.help || !command) {
    process.stdout.write(USAGE);
    process.exit(command ? 0 : 2);
  }
  for (const k of ['port', 'client-port']) {
    if (flags[k] !== undefined && !/^\d{1,5}$/.test(flags[k])) fail(`--${k} must be a port number`);
  }

  switch (command) {
    case 'doctor':
      return cmdDoctor();
    case 'full-build-dev':
    case 'dev':
      return cmdFullBuildDev(flags);
    case 'full-build-app':
    case 'app':
      return cmdFullBuildApp(flags);
    case 'full-build-apk':
    case 'apk':
      return cmdFullBuildApk(flags);
    case 'engine':
      return cmdEngine(flags);
    case 'client':
      return cmdClient(flags);
    default:
      process.stderr.write(`unknown command '${command}'\n\n${USAGE}`);
      process.exit(2);
  }
}

main().catch((err) => fail(err?.stack || String(err)));
