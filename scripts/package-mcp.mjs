#!/usr/bin/env node
// package-mcp.mjs — build portable `bebok-server mcp` bundles for Claude Code.
//
// Produces dist/bebok-mcp-<version>-<platform>.{zip,tar.gz}, each holding the
// release binary plus an installer that registers it with Claude Code at user
// scope (`claude mcp add bebok -s user -- <bin> mcp`). No --project is baked
// in: Claude Code starts stdio servers in the session's working directory and
// `bebok-server mcp` defaults its project to the cwd, so one registration
// serves every project.
//
//   node scripts/package-mcp.mjs [--targets linux,windows,macos] [--skip-build]
//
// Default targets: the host platform, plus Windows when building on Linux.
//
// Windows from Linux: needs `cargo-zigbuild` (cargo install --locked
// cargo-zigbuild; zig on PATH) or a mingw-w64 gcc (x86_64-w64-mingw32-gcc).
// Linux is built with plain cargo against the host glibc + OpenSSL 3 (needs
// libssl-dev), so it runs on distros at least as new as the build host.

import { spawnSync } from 'node:child_process';
import {
  chmodSync, copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync,
} from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = fileURLToPath(new URL('..', import.meta.url));
const ENGINE = join(ROOT, 'engine');
const DIST = join(ROOT, 'dist');

const argv = process.argv.slice(2);
const skipBuild = argv.includes('--skip-build');
const targetsArg = (() => {
  const i = argv.indexOf('--targets');
  return i >= 0 ? argv[i + 1] : null;
})();

function fail(msg, hint) {
  console.error(`✗ ${msg}`);
  if (hint) console.error(`  ${hint}`);
  process.exit(1);
}

function has(cmd, args = ['--version']) {
  const r = spawnSync(cmd, args, { encoding: 'utf8', shell: process.platform === 'win32' });
  return !r.error && r.status === 0;
}

function run(cmd, args, opts = {}) {
  console.log(`· ${cmd} ${args.join(' ')}`);
  const r = spawnSync(cmd, args, { stdio: 'inherit', shell: process.platform === 'win32', ...opts });
  if (r.error || r.status !== 0) fail(`${cmd} failed`);
}

const version = (() => {
  const toml = readFileSync(join(ENGINE, 'crates', 'bebok-server', 'Cargo.toml'), 'utf8');
  const m = toml.match(/^version\s*=\s*"([^"]+)"/m);
  if (m) return m[1];
  const ws = readFileSync(join(ENGINE, 'Cargo.toml'), 'utf8').match(/^version\s*=\s*"([^"]+)"/m);
  return ws ? ws[1] : '0.0.0';
})();

const host = { linux: 'linux', win32: 'windows', darwin: 'macos' }[process.platform];
if (!host) fail(`unsupported host ${process.platform}`);

const wanted = targetsArg
  ? targetsArg.split(',').map((s) => s.trim()).filter(Boolean)
  : host === 'linux' ? ['linux', 'windows'] : [host];

const zigbuild = has('cargo', ['zigbuild', '--help']);

// ---------------------------------------------------------------------------
// per-target build plans
// ---------------------------------------------------------------------------

function plan(platform) {
  const arch = process.arch === 'arm64' ? 'aarch64' : 'x86_64';
  switch (platform) {
    case 'linux': {
      if (host !== 'linux') fail('linux bundles must be built on Linux');
      // Plain cargo, not zigbuild: openssl-sys needs the host's OpenSSL headers
      // (Debian/Ubuntu keep opensslconf.h under /usr/include/<multiarch>/),
      // which zig's sysroot does not see.
      const triple = `${arch}-unknown-linux-gnu`;
      return { triple, cmd: 'build', buildTarget: triple, exe: 'bebok-server', env: {} };
    }
    case 'macos': {
      if (host !== 'macos') fail('macOS bundles must be built on macOS');
      const triple = `${arch}-apple-darwin`;
      return { triple, cmd: 'build', buildTarget: triple, exe: 'bebok-server', env: {} };
    }
    case 'windows': {
      if (host === 'windows') {
        const triple = 'x86_64-pc-windows-msvc';
        // Static CRT: no "VCRUNTIME140.dll missing" on a clean machine.
        return {
          triple, cmd: 'build', buildTarget: triple, exe: 'bebok-server.exe',
          env: { [`CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS`]: '-C target-feature=+crt-static' },
        };
      }
      const triple = 'x86_64-pc-windows-gnu';
      const rustflags = '-C target-feature=+crt-static';
      if (zigbuild) {
        return {
          triple, cmd: 'zigbuild', buildTarget: triple, exe: 'bebok-server.exe',
          env: { CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS: rustflags },
        };
      }
      if (has('x86_64-w64-mingw32-gcc')) {
        return {
          triple, cmd: 'build', buildTarget: triple, exe: 'bebok-server.exe',
          env: {
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER: 'x86_64-w64-mingw32-gcc',
            CC_x86_64_pc_windows_gnu: 'x86_64-w64-mingw32-gcc',
            AR_x86_64_pc_windows_gnu: 'x86_64-w64-mingw32-ar',
            CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS: `${rustflags} -C link-args=-static`,
          },
        };
      }
      fail(
        'no Windows cross toolchain found.',
        'Install one: `cargo install --locked cargo-zigbuild` (zig must be on PATH) ' +
          'or `sudo apt install mingw-w64`. Or run this script on Windows.',
      );
    }
    // eslint-disable-next-line no-fallthrough
    default:
      fail(`unknown target "${platform}" (linux | windows | macos)`);
  }
}

// ---------------------------------------------------------------------------
// bundle contents
// ---------------------------------------------------------------------------

const README = (platform, exe) => `# Bebok MCP server ${version} (${platform})

Exposes Bebok's tools (file tools, bash, browser, ssh, code_index / code_ast /
code_graph when enabled in the project) to Claude Code over MCP stdio.
Runs in YOLO mode: Claude Code's own permission prompts are the only gate.

## Install

${platform === 'windows'
    ? 'Double-click `install.cmd` (or run `powershell -ExecutionPolicy Bypass -File install.ps1`).'
    : 'Run `./install.sh`.'}

The installer copies \`${exe}\` to a stable location
(${platform === 'windows' ? '`%LOCALAPPDATA%\\bebok-mcp\\`' : '`~/.local/share/bebok-mcp/`'})
and registers it with Claude Code for all projects:

    claude mcp add bebok -s user -- <path>/${exe} mcp

Restart Claude Code, then check \`/mcp\` → \`bebok ✓ connected\`.

The project is the directory Claude Code was started in. Options (append after
\`mcp\` when registering manually):

- \`--project DIR\`   pin a project directory
- \`--read-only\`     expose only non-mutating tools
- \`--exclude a,b\`   hide tools by name (e.g. \`bash,ssh\`)

## Requirements

- Claude Code (\`claude\` on PATH) for the installer.
${platform === 'linux' ? '- glibc-based Linux with OpenSSL 3 (`libssl.so.3`) — any current distro.\n' : ''}- Browser tools need Chrome/Chromium installed (or \`BEBOK_BROWSER=<path>\`).
- \`code_index_*\` needs the \`bebok-index\` plugin installed in the project's \`.bebok/plugins/\`.

## Uninstall

    claude mcp remove bebok -s user

and delete the install directory.
`;

const INSTALL_SH = `#!/bin/sh
set -e
here=$(cd "$(dirname "$0")" && pwd)
dest="\${XDG_DATA_HOME:-$HOME/.local/share}/bebok-mcp"
mkdir -p "$dest"
cp "$here/bebok-server" "$dest/bebok-server"
chmod +x "$dest/bebok-server"
case "$(uname -s)" in Darwin) xattr -d com.apple.quarantine "$dest/bebok-server" 2>/dev/null || true ;; esac
echo "installed: $dest/bebok-server"
if ! command -v claude >/dev/null 2>&1; then
  echo "claude not found on PATH - register manually:"
  echo "  claude mcp add bebok -s user -- \\"$dest/bebok-server\\" mcp"
  exit 0
fi
claude mcp remove bebok -s user >/dev/null 2>&1 || true
claude mcp add bebok -s user -- "$dest/bebok-server" mcp
echo "done - restart Claude Code and check /mcp"
`;

const INSTALL_PS1 = `$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$dest = Join-Path $env:LOCALAPPDATA 'bebok-mcp'
New-Item -ItemType Directory -Force -Path $dest | Out-Null
$exe = Join-Path $dest 'bebok-server.exe'
Copy-Item -Force (Join-Path $here 'bebok-server.exe') $exe
Unblock-File -Path $exe -ErrorAction SilentlyContinue
Write-Host "installed: $exe"
if (-not (Get-Command claude -ErrorAction SilentlyContinue)) {
  Write-Host 'claude not found on PATH - register manually:'
  Write-Host "  claude mcp add bebok -s user -- \`"$exe\`" mcp"
  exit 0
}
& claude mcp remove bebok -s user 2>$null | Out-Null
& claude mcp add bebok -s user -- "$exe" mcp
if ($LASTEXITCODE -ne 0) { throw 'claude mcp add failed' }
Write-Host 'done - restart Claude Code and check /mcp'
`;

const INSTALL_CMD = `@echo off\r
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0install.ps1"\r
pause\r
`;

// ---------------------------------------------------------------------------
// build + package
// ---------------------------------------------------------------------------

mkdirSync(DIST, { recursive: true });
const produced = [];

for (const platform of wanted) {
  const p = plan(platform);
  if (!skipBuild) {
    if (has('rustup', ['--version'])) run('rustup', ['target', 'add', p.triple]);
    run('cargo', [p.cmd, '--release', '-p', 'bebok-server', '--target', p.buildTarget], {
      cwd: ENGINE,
      env: { ...process.env, ...p.env },
    });
  }
  const built = join(ENGINE, 'target', p.triple, 'release', p.exe);
  if (!existsSync(built)) fail(`binary not found: ${built}`);

  const name = `bebok-mcp-${version}-${platform}-${p.triple.split('-')[0]}`;
  const dir = join(DIST, name);
  rmSync(dir, { recursive: true, force: true });
  mkdirSync(dir, { recursive: true });
  copyFileSync(built, join(dir, p.exe));
  writeFileSync(join(dir, 'README.md'), README(platform, p.exe));
  if (platform === 'windows') {
    writeFileSync(join(dir, 'install.ps1'), INSTALL_PS1);
    writeFileSync(join(dir, 'install.cmd'), INSTALL_CMD);
  } else {
    chmodSync(join(dir, p.exe), 0o755);
    writeFileSync(join(dir, 'install.sh'), INSTALL_SH);
    chmodSync(join(dir, 'install.sh'), 0o755);
  }

  // Archive: zip for Windows (Explorer opens it natively), tar.gz elsewhere.
  let archive;
  if (platform === 'windows') {
    archive = join(DIST, `${name}.zip`);
    rmSync(archive, { force: true });
    if (host === 'windows') {
      run('powershell', ['-NoProfile', '-Command', `Compress-Archive -Path '${dir}' -DestinationPath '${archive}'`]);
    } else if (has('zip', ['-v'])) {
      run('zip', ['-qr', archive, name], { cwd: DIST });
    } else {
      run('python3', ['-c', 'import shutil,sys; shutil.make_archive(sys.argv[1], "zip", sys.argv[2], sys.argv[3])',
        archive.replace(/\.zip$/, ''), DIST, name]);
    }
  } else {
    archive = join(DIST, `${name}.tar.gz`);
    run('tar', ['-czf', archive, '-C', DIST, name]);
  }
  produced.push(archive);
}

console.log('\n✓ bundles:');
for (const a of produced) console.log(`  ${a}`);
