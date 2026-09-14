#!/usr/bin/env node
// release.mjs — guided release for Bebok (Linux / macOS / Windows, Node >= 20, zero deps).
//
//   npm run release -- [X.Y.Z] [--yes] [--no-pr]   start a release   (node scripts/release.mjs …)
//   npm run release:status -- [X.Y.Z]              where is the release right now?
//
// The whole process is: this script opens a release PR, CI builds a draft
// release from it (install it, test it), merging the PR publishes the release
// automatically and installed apps update themselves. Nothing here talks to
// GitHub Releases directly - the workflow owns that (see CONTRIBUTING.md).
//
// What "start" does, asking before every step unless --yes:
//   1. checks git, gh (logged in), a clean tree and that origin/<default> is current
//   2. picks the version (argument, or a suggestion from client/package.json)
//   3. turns "## Unreleased" in CHANGELOG.md into "## X.Y.Z — YYYY-MM-DD"
//   4. runs `npm run version:bump -- X.Y.Z` (seven manifests/lockfiles)
//   5. commits on release/X.Y.Z, pushes, opens the PR (`--no-pr` prints the command instead)

import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { createInterface } from 'node:readline/promises';
import { fileURLToPath } from 'node:url';

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const CLIENT = join(ROOT, 'client');
const CHANGELOG = join(ROOT, 'CHANGELOG.md');
const isWin = process.platform === 'win32';

const argv = process.argv.slice(2);
const flags = new Set(argv.filter((a) => a.startsWith('--')));
const words = argv.filter((a) => !a.startsWith('--'));
const yes = flags.has('--yes') || flags.has('-y');

const c = {
  bold: (s) => `\x1b[1m${s}\x1b[0m`,
  dim: (s) => `\x1b[2m${s}\x1b[0m`,
  green: (s) => `\x1b[32m${s}\x1b[0m`,
  yellow: (s) => `\x1b[33m${s}\x1b[0m`,
  red: (s) => `\x1b[31m${s}\x1b[0m`,
};
const log = (m) => console.log(m);
const ok = (m) => log(`${c.green('✓')} ${m}`);
const warn = (m) => log(`${c.yellow('!')} ${m}`);
function fail(m) {
  console.error(`${c.red('✗')} ${m}`);
  process.exit(1);
}

// ---------------------------------------------------------------------------
// process helpers
// ---------------------------------------------------------------------------

function run(cmd, args, { cwd = ROOT, check = true, quiet = false } = {}) {
  const r = spawnSync(cmd, args, { cwd, encoding: 'utf8', windowsHide: true });
  if (r.error) {
    if (check) fail(`${cmd} ${args.join(' ')}: ${r.error.message}`);
    return { status: -1, stdout: '', stderr: r.error.message };
  }
  if (check && r.status !== 0) {
    if (!quiet) process.stderr.write(r.stderr || r.stdout || '');
    fail(`${cmd} ${args.join(' ')} exited with ${r.status}`);
  }
  return { status: r.status, stdout: (r.stdout || '').trim(), stderr: (r.stderr || '').trim() };
}

const git = (...args) => run('git', args).stdout;
const gitTry = (...args) => run('git', args, { check: false, quiet: true });
// gh is a real executable on every OS; npm is npm.cmd on Windows and needs a shell.
const gh = (...args) => run(isWin ? 'gh.exe' : 'gh', args);
const ghTry = (...args) => run(isWin ? 'gh.exe' : 'gh', args, { check: false, quiet: true });
function npm(args, cwd) {
  if (isWin) return run('cmd.exe', ['/d', '/s', '/c', `npm ${args.join(' ')}`], { cwd });
  return run('npm', args, { cwd });
}

let rl = null;
async function ask(question, fallback) {
  if (yes) return fallback;
  rl ??= createInterface({ input: process.stdin, output: process.stdout });
  const answer = (await rl.question(`${question}${fallback !== undefined ? c.dim(` [${fallback}]`) : ''} `)).trim();
  return answer || fallback;
}
async function confirm(question) {
  if (yes) return true;
  const answer = (await ask(`${question} (y/N)`, 'n')).toLowerCase();
  return answer === 'y' || answer === 'yes';
}

// ---------------------------------------------------------------------------
// repo facts
// ---------------------------------------------------------------------------

const VERSION_RE = /^[0-9]+\.[0-9]+\.[0-9]+(-[0-9]{1,5})?$/;

function manifestVersion() {
  return JSON.parse(readFileSync(join(CLIENT, 'package.json'), 'utf8')).version;
}

function defaultBranch() {
  const ref = gitTry('symbolic-ref', '--short', 'refs/remotes/origin/HEAD').stdout;
  if (ref) return ref.replace(/^origin\//, '');
  return gitTry('rev-parse', '--verify', '--quiet', 'origin/main').status === 0 ? 'main' : 'master';
}

function repoSlug() {
  const out = ghTry('repo', 'view', '--json', 'nameWithOwner', '--jq', '.nameWithOwner').stdout;
  if (out) return out;
  const url = git('remote', 'get-url', 'origin');
  const m = url.match(/github\.com[:/]([^/]+\/[^/.]+)/);
  return m ? m[1] : null;
}

function tagExists(tag) {
  return gitTry('ls-remote', '--exit-code', '--tags', 'origin', `refs/tags/${tag}`).status === 0;
}

function bump(version, part) {
  const [major, minor, patch] = version.split('-')[0].split('.').map(Number);
  if (part === 'major') return `${major + 1}.0.0`;
  if (part === 'minor') return `${major}.${minor + 1}.0`;
  return `${major}.${minor}.${patch + 1}`;
}

function unreleasedSection(markdown) {
  const m = markdown.match(/^## Unreleased[^\n]*\n([\s\S]*?)(?=^## |(?![\s\S]))/m);
  return m ? { heading: m[0].split('\n')[0], body: m[1].trim(), full: m[0] } : null;
}

function isoDate() {
  return new Date().toISOString().slice(0, 10);
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

async function start() {
  log(c.bold('\nBebok release\n'));

  // 1. tools + tree
  if (gitTry('--version').status !== 0) fail('git is not installed');
  if (ghTry('--version').status !== 0) fail('GitHub CLI (gh) is not installed: https://cli.github.com');
  if (ghTry('auth', 'status').status !== 0) fail('gh is not logged in - run: gh auth login');
  ok('git + gh ready');

  const dirty = git('status', '--porcelain');
  if (dirty) fail(`working tree is not clean:\n${dirty}\nCommit or stash first.`);
  git('fetch', '--prune', '--tags', 'origin');
  const base = defaultBranch();
  const current = git('rev-parse', '--abbrev-ref', 'HEAD');
  if (current !== base) {
    warn(`you are on '${current}', releases start from '${base}'`);
    if (!(await confirm(`Switch to ${base}?`))) fail('aborted');
    git('checkout', base);
  }
  const behind = Number(gitTry('rev-list', '--count', `HEAD..origin/${base}`).stdout || 0);
  const ahead = Number(gitTry('rev-list', '--count', `origin/${base}..HEAD`).stdout || 0);
  if (ahead > 0) fail(`${base} has ${ahead} local commit(s) not on origin - push or drop them first`);
  if (behind > 0) {
    git('merge', '--ff-only', `origin/${base}`);
    ok(`fast-forwarded ${base} by ${behind} commit(s)`);
  }
  ok(`on ${base} at ${git('rev-parse', '--short', 'HEAD')} (in sync with origin)`);

  const ciRun = ghTry('run', 'list', '--workflow', 'ci.yml', '--branch', base, '--limit', '1', '--json', 'conclusion', '--jq', '.[0].conclusion').stdout;
  if (ciRun && ciRun !== 'success') {
    warn(`last CI run on ${base}: ${ciRun}`);
    if (!(await confirm('Continue anyway?'))) fail('aborted');
  } else if (ciRun) {
    ok(`CI on ${base} is green`);
  }

  // 2. version
  const currentVersion = manifestVersion();
  let version = words[1] ?? (words[0] && words[0] !== 'start' ? words[0] : null);
  if (!version) {
    log(`\ncurrent version: ${c.bold(currentVersion)}   patch → ${bump(currentVersion, 'patch')}   minor → ${bump(currentVersion, 'minor')}   major → ${bump(currentVersion, 'major')}`);
    version = await ask('Release version (X.Y.Z)', bump(currentVersion, 'patch'));
  }
  version = version.replace(/^v/, '');
  if (!VERSION_RE.test(version)) fail(`'${version}' is not X.Y.Z (a pre-release suffix must be numeric: X.Y.Z-1)`);
  if (version === currentVersion) fail(`manifests are already at ${version} - was this released? (tag exists: ${tagExists(version)})`);
  if (tagExists(version)) fail(`tag ${version} already exists on origin - pick the next version`);
  const branch = `release/${version}`;
  if (gitTry('rev-parse', '--verify', '--quiet', `origin/${branch}`).status === 0) fail(`branch ${branch} already exists on origin - continue there or delete it`);
  ok(`releasing ${c.bold(version)} (from ${currentVersion}) on ${branch}`);

  // 3. changelog
  const changelog = readFileSync(CHANGELOG, 'utf8');
  const section = unreleasedSection(changelog);
  if (!section) {
    fail(`CHANGELOG.md has no "## Unreleased" section. Add one with this release's entries first.`);
  }
  if (!section.body) fail('the "## Unreleased" section is empty - write the changelog first');
  log(`\n${c.dim('--- CHANGELOG.md → ## ' + version + ' — ' + isoDate() + ' ---')}\n${section.body}\n${c.dim('---')}`);
  log(c.dim('(these notes become the update notes users see in the app)'));
  if (!(await confirm('Changelog looks right?'))) fail('aborted - edit CHANGELOG.md and run again');
  writeFileSync(CHANGELOG, changelog.replace(section.heading, `## ${version} — ${isoDate()}`));
  ok('CHANGELOG.md updated');

  // 4. bump
  npm(['run', 'version:bump', '--', version], CLIENT);
  ok('version bumped in all manifests');

  // 5. branch, commit, push, PR
  git('checkout', '-b', branch);
  git('add', '-A');
  git('commit', '-q', '-m', `chore: release ${version}`);
  ok(`committed on ${branch}`);
  if (!(await confirm(`Push ${branch} and open the release PR?`))) {
    warn(`branch ${branch} is committed locally only. Push with: git push -u origin ${branch}`);
    return;
  }
  git('push', '-q', '-u', 'origin', branch);
  ok('pushed');

  const body = [
    `Release **${version}**.`,
    '',
    '- CI builds a **draft release** from this PR (Actions → Release). Install it from the Releases page to test.',
    `- Merging publishes ${version}: the workflow tags the merge commit, publishes the release and installed apps update automatically.`,
    '- Do not upload or edit release assets by hand.',
    '',
    '### Changelog',
    '',
    section.body,
  ].join('\n');
  if (flags.has('--no-pr')) {
    log(`\nOpen the PR yourself:\n  gh pr create --base ${base} --head ${branch} --title "release: ${version}" --body-file <notes>`);
    return;
  }
  const pr = gh('pr', 'create', '--base', base, '--head', branch, '--title', `release: ${version}`, '--body', body).stdout;
  ok(`release PR: ${pr}`);

  log(`
${c.bold('Next')}
  1. Actions → Release builds a draft "${version}" (~30 min):   gh run watch
  2. Install the draft on a machine with the previous version and check the update path.
  3. Merge the PR → the release is tagged and published automatically.
  4. Verify:   npm run release:status -- ${version}
`);
}

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

function status() {
  const version = (words[1] ?? manifestVersion()).replace(/^v/, '');
  const slug = repoSlug();
  log(c.bold(`\nBebok release ${version}\n`));

  const pr = ghTry('pr', 'list', '--head', `release/${version}`, '--state', 'all', '--json', 'number,state,url,mergedAt', '--jq', '.[0]').stdout;
  if (pr) {
    const p = JSON.parse(pr);
    log(`PR:        #${p.number} ${p.state}${p.mergedAt ? ` (merged ${p.mergedAt})` : ''}  ${p.url}`);
  } else {
    log(`PR:        none for release/${version}`);
  }

  log(`Tag:       ${tagExists(version) ? c.green('exists') : c.dim('not yet')}`);

  const rel = ghTry('release', 'view', version, '--json', 'isDraft,isPrerelease,publishedAt,url,assets', '--jq', '{d:.isDraft,p:.isPrerelease,at:.publishedAt,url:.url,n:(.assets|length),m:([.assets[].name]|index("latest.json")!=null)}').stdout;
  if (rel) {
    const r = JSON.parse(rel);
    log(`Release:   ${r.d ? c.yellow('DRAFT') : r.p ? c.yellow('pre-release') : c.green('published')}  ${r.n} assets, latest.json ${r.m ? c.green('present') : c.red('MISSING')}  ${r.url}`);
  } else {
    log(`Release:   ${c.dim('none')}`);
  }

  const runs = ghTry('run', 'list', '--workflow', 'release.yml', '--limit', '3', '--json', 'status,conclusion,headBranch,event,url', '--jq', '.[] | "\\(.status)\\t\\(.conclusion // "-")\\t\\(.event)\\t\\(.headBranch)\\t\\(.url)"').stdout;
  if (runs) log(`Runs:\n${runs.split('\n').map((l) => `           ${l}`).join('\n')}`);

  if (slug) {
    const feed = run('curl', ['-sL', '-o', '-', '-w', '\\n%{http_code}', `https://github.com/${slug}/releases/latest/download/latest.json`], { check: false, quiet: true }).stdout;
    const lines = feed.split('\n');
    const code = lines.pop();
    if (code === '200') {
      try {
        const m = JSON.parse(lines.join('\n'));
        const live = m.version === version;
        log(`Feed:      latest.json → ${live ? c.green(m.version) : c.yellow(m.version)}  (${Object.keys(m.platforms ?? {}).length} targets)${live ? '  ← installed apps will pick this up' : ''}`);
      } catch {
        log(`Feed:      latest.json → ${c.red('unparseable')}`);
      }
    } else {
      log(`Feed:      latest.json → HTTP ${code} ${c.dim('(no published release with a manifest yet)')}`);
    }
  }
  log('');
}

// ---------------------------------------------------------------------------

try {
  if (words[0] === 'status') status();
  else if (flags.has('--help') || flags.has('-h')) {
    log(readFileSync(fileURLToPath(import.meta.url), 'utf8').split('\n').slice(1, 18).map((l) => l.replace(/^\/\/ ?/, '')).join('\n'));
  } else await start();
} finally {
  rl?.close();
}
