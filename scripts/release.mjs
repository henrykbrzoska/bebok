#!/usr/bin/env node
// release.mjs — guided release for Bebok (Linux / macOS / Windows, Node >= 20, zero deps).
//
//   npm run release:check                          am I ready to release? (read-only)
//   npm run release -- [X.Y.Z] [--yes] [--no-pr]   start a release   (node scripts/release.mjs …)
//   npm run release:status -- [X.Y.Z]              where is the release right now?
//
// The whole process is: this script opens a release PR, CI builds a draft
// release from it (install it, test it), merging the PR publishes the release
// automatically and installed apps update themselves. Nothing here talks to
// GitHub Releases directly - the workflow owns that (see CONTRIBUTING.md).
//
// What "start" does, asking before every step unless --yes:
//   1. runs the readiness check (git, gh, clean tree, main in sync, CI green,
//      changelog has entries, signing secret present, no release PR open)
//   2. picks the version: argument, or the semver suggested by the commits since the
//      last tag (feat! / BREAKING CHANGE -> major, feat -> minor, anything else -> patch)
//   3. turns "## Unreleased" in CHANGELOG.md into "## X.Y.Z — YYYY-MM-DD"; when the
//      section is missing or empty it drafts one from those commits for you to edit
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

// ---------------------------------------------------------------------------
// commits -> semver + changelog draft (Conventional Commits, loosely)
// ---------------------------------------------------------------------------

const CONVENTIONAL_RE = /^(?<type>[a-z]+)(?:\((?<scope>[^)]*)\))?(?<bang>!)?:\s*(?<subject>.+)$/;

/** Commits on `ref` since `tag` (all of them when there is no tag), merges skipped. */
function commitsSince(tag, ref) {
  const range = tag ? `${tag}..${ref}` : ref;
  const raw = gitTry('log', '--no-merges', '--format=%H%x1f%s%x1f%b%x1e', range).stdout;
  if (!raw) return [];
  return raw
    .split('\x1e')
    .map((entry) => entry.trim())
    .filter(Boolean)
    .map((entry) => {
      const [hash, subject, body = ''] = entry.split('\x1f');
      const m = subject.match(CONVENTIONAL_RE);
      const type = m ? m.groups.type : null;
      const breaking = Boolean(m?.groups.bang) || /^BREAKING[ -]CHANGE:/m.test(body);
      return { hash: hash.slice(0, 7), subject, type, scope: m?.groups.scope ?? null, text: m ? m.groups.subject : subject, breaking };
    });
}

function classify(commits) {
  const groups = { breaking: [], feat: [], fix: [], other: [] };
  for (const commit of commits) {
    if (commit.breaking) groups.breaking.push(commit);
    else if (commit.type === 'feat') groups.feat.push(commit);
    else if (commit.type === 'fix' || commit.type === 'perf') groups.fix.push(commit);
    else groups.other.push(commit);
  }
  const part = groups.breaking.length ? 'major' : groups.feat.length ? 'minor' : 'patch';
  return { groups, part };
}

function suggestVersion(current, commits) {
  const { part, groups } = classify(commits);
  const counts = `${groups.breaking.length} breaking, ${groups.feat.length} feat, ${groups.fix.length} fix, ${groups.other.length} other`;
  // A hand-made tag may already sit on the computed version (1.6.1 was): skip past it.
  let version = bump(current, part);
  while (tagExists(version)) version = bump(version, 'patch');
  return { version, part, counts };
}

function changelogDraft(commits) {
  const { groups } = classify(commits);
  const line = (commit) => `- ${commit.scope ? `**${commit.scope}**: ` : ''}${commit.text} (${commit.hash})`;
  const blocks = [];
  if (groups.breaking.length) blocks.push(['### Breaking changes', ...groups.breaking.map(line)]);
  if (groups.feat.length) blocks.push(['### Features', ...groups.feat.map(line)]);
  if (groups.fix.length) blocks.push(['### Fixes', ...groups.fix.map(line)]);
  if (groups.other.length) blocks.push(['### Other', ...groups.other.map(line)]);
  return blocks.map((b) => b.join('\n')).join('\n\n');
}

function isoDate() {
  return new Date().toISOString().slice(0, 10);
}

// ---------------------------------------------------------------------------
// readiness check (read-only)
// ---------------------------------------------------------------------------

function lastTag() {
  const tags = gitTry('tag', '--list', '--sort=-v:refname').stdout.split('\n').filter((t) => VERSION_RE.test(t.replace(/^v/, '')));
  return tags[0] ?? null;
}

/** Every item: { ok, label, detail?, fatal? }. `fatal: false` items are warnings. */
function readiness() {
  const items = [];
  const add = (ok, label, detail = '', fatal = true) => items.push({ ok, label, detail, fatal });

  const hasGit = gitTry('--version').status === 0;
  add(hasGit, 'git installed');
  const hasGh = ghTry('--version').status === 0;
  add(hasGh, 'gh installed', hasGh ? '' : 'https://cli.github.com');
  const ghAuth = hasGh && ghTry('auth', 'status').status === 0;
  add(ghAuth, 'gh logged in', ghAuth ? '' : 'gh auth login');
  if (!hasGit) return items;

  const dirty = gitTry('status', '--porcelain').stdout;
  add(!dirty, 'working tree clean', dirty ? `${dirty.split('\n').length} changed file(s)` : '');

  gitTry('fetch', '--prune', '--tags', 'origin');
  const base = defaultBranch();
  const current = gitTry('rev-parse', '--abbrev-ref', 'HEAD').stdout;
  add(current === base, `on ${base}`, current === base ? '' : `on '${current}' - the script can switch for you`, false);
  const hasLocalBase = gitTry('rev-parse', '--verify', '--quiet', base).status === 0;
  const behind = hasLocalBase ? Number(gitTry('rev-list', '--count', `${base}..origin/${base}`).stdout || 0) : 0;
  const ahead = hasLocalBase ? Number(gitTry('rev-list', '--count', `origin/${base}..${base}`).stdout || 0) : 0;
  add(ahead === 0, `local ${base} has no unpushed commits`, ahead ? `${ahead} commit(s) ahead of origin/${base} - push or drop them` : '');
  add(true, `local ${base} ${behind ? `is ${behind} commit(s) behind origin (will fast-forward)` : 'in sync with origin'}`, '', false);

  if (ghAuth) {
    const ci = ghTry('run', 'list', '--workflow', 'ci.yml', '--branch', base, '--limit', '1', '--json', 'conclusion,url', '--jq', '.[0] | "\\(.conclusion) \\(.url)"').stdout;
    const [conclusion, url] = ci.split(' ');
    add(conclusion === 'success', `CI green on ${base}`, conclusion === 'success' ? '' : `${conclusion || 'no run'} ${url || ''}`.trim(), false);

    const secrets = ghTry('secret', 'list', '--json', 'name', '--jq', '.[].name').stdout.split('\n');
    add(secrets.includes('TAURI_SIGNING_PRIVATE_KEY'), 'TAURI_SIGNING_PRIVATE_KEY secret set', secrets.includes('TAURI_SIGNING_PRIVATE_KEY') ? '' : 'Settings -> Secrets and variables -> Actions (or no permission to list secrets)', false);

    const openPr = ghTry('pr', 'list', '--state', 'open', '--json', 'headRefName,url', '--jq', '.[] | select(.headRefName | startswith("release/")) | "\\(.headRefName) \\(.url)"').stdout;
    add(!openPr, 'no release PR already open', openPr);
  }

  const changelog = existsSync(CHANGELOG) ? readFileSync(CHANGELOG, 'utf8') : '';
  const section = unreleasedSection(changelog);
  const entries = section ? section.body.split('\n').filter((l) => /^\s*[-*]/.test(l)).length : 0;
  add(Boolean(section && section.body), 'CHANGELOG.md has a filled "## Unreleased" section', section ? (section.body ? `${entries} bullet(s)` : 'empty - `release` drafts it from the commits') : 'missing - `release` drafts it from the commits', false);

  const tag = lastTag();
  const commits = commitsSince(tag, `origin/${base}`);
  const version = manifestVersion();
  add(commits.length > 0, `commits on ${base} since last tag${tag ? ` ${tag}` : ''}`, commits.length ? `${commits.length}` : 'nothing new to release', false);
  const suggestion = suggestVersion(version, commits);
  add(true, `manifest version ${version}${tagExists(version) ? ' (released)' : ' (not tagged yet)'} -> suggested next: ${c.bold(suggestion.version)} (${suggestion.part}: ${suggestion.counts})`, '', false);

  return items;
}

function printReadiness(items) {
  for (const { ok, label, detail, fatal } of items) {
    const mark = ok ? c.green('✓') : fatal ? c.red('✗') : c.yellow('!');
    log(`${mark} ${label}${detail ? c.dim(`  ${detail}`) : ''}`);
  }
  const blockers = items.filter((i) => !i.ok && i.fatal);
  const warnings = items.filter((i) => !i.ok && !i.fatal);
  log('');
  if (blockers.length) log(c.red(`${blockers.length} blocker(s) - fix them and run again.`));
  else if (warnings.length) log(c.yellow(`Ready with ${warnings.length} warning(s).`));
  else log(c.green('Ready to release.'));
  return blockers.length === 0;
}

function check() {
  log(c.bold('\nBebok release - readiness\n'));
  const ready = printReadiness(readiness());
  if (ready) log(c.dim('Next: npm run release -- X.Y.Z'));
  process.exitCode = ready ? 0 : 1;
}

// ---------------------------------------------------------------------------
// start
// ---------------------------------------------------------------------------

async function start() {
  log(c.bold('\nBebok release\n'));

  // 1. readiness
  if (!printReadiness(readiness())) fail('not ready');
  const base = defaultBranch();
  const current = git('rev-parse', '--abbrev-ref', 'HEAD');
  if (current !== base) {
    if (!(await confirm(`Switch to ${base}?`))) fail('aborted');
    git('checkout', base);
  }
  const behind = Number(gitTry('rev-list', '--count', `HEAD..origin/${base}`).stdout || 0);
  if (behind > 0) {
    git('merge', '--ff-only', `origin/${base}`);
    ok(`fast-forwarded ${base} by ${behind} commit(s)`);
  }
  const warnings = readiness().filter((i) => !i.ok && !i.fatal);
  if (warnings.length && !(await confirm('Continue despite the warnings above?'))) fail('aborted');

  // 2. version
  const currentVersion = manifestVersion();
  const commits = commitsSince(lastTag(), 'HEAD');
  const suggestion = suggestVersion(currentVersion, commits);
  let version = words[1] ?? (words[0] && words[0] !== 'start' ? words[0] : null);
  if (!version) {
    log(`\ncurrent version: ${c.bold(currentVersion)}   ${commits.length} commit(s) since the last tag: ${suggestion.counts}`);
    log(`suggested: ${c.bold(suggestion.version)} (${suggestion.part})   patch → ${bump(currentVersion, 'patch')}   minor → ${bump(currentVersion, 'minor')}   major → ${bump(currentVersion, 'major')}`);
    version = await ask('Release version (X.Y.Z)', suggestion.version);
  } else if (version.replace(/^v/, '') !== suggestion.version && commits.length) {
    warn(`commits since the last tag suggest ${suggestion.version} (${suggestion.part}: ${suggestion.counts}); you chose ${version}`);
  }
  version = version.replace(/^v/, '');
  if (!VERSION_RE.test(version)) fail(`'${version}' is not X.Y.Z (a pre-release suffix must be numeric: X.Y.Z-1)`);
  if (version === currentVersion) fail(`manifests are already at ${version} - was this released? (tag exists: ${tagExists(version)})`);
  if (tagExists(version)) fail(`tag ${version} already exists on origin - pick the next version`);
  const branch = `release/${version}`;
  if (gitTry('rev-parse', '--verify', '--quiet', `origin/${branch}`).status === 0) fail(`branch ${branch} already exists on origin - continue there or delete it`);
  ok(`releasing ${c.bold(version)} (from ${currentVersion}) on ${branch}`);

  // 3. changelog
  let changelog = readFileSync(CHANGELOG, 'utf8');
  let section = unreleasedSection(changelog);
  if (!section || !section.body) {
    if (!commits.length) fail('CHANGELOG.md has no "## Unreleased" entries and there are no commits since the last tag');
    const draft = changelogDraft(commits);
    log(`\n${c.dim('--- draft from the commits since the last tag ---')}\n${draft}\n${c.dim('---')}`);
    if (!(await confirm(section ? 'The "## Unreleased" section is empty - fill it with this draft?' : 'CHANGELOG.md has no "## Unreleased" section - add it with this draft?'))) {
      fail('aborted - write the "## Unreleased" section in CHANGELOG.md and run again');
    }
    changelog = section
      ? changelog.replace(section.heading, `${section.heading}\n\n${draft}\n`)
      : changelog.replace(/^(# [^\n]*\n)/, `$1\n## Unreleased\n\n${draft}\n`);
    writeFileSync(CHANGELOG, changelog);
    section = unreleasedSection(changelog);
    warn('draft written to CHANGELOG.md - edit it now if you want, then confirm below');
  }
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
  else if (words[0] === 'check') check();
  else if (flags.has('--help') || flags.has('-h')) {
    log(readFileSync(fileURLToPath(import.meta.url), 'utf8').split('\n').slice(1, 18).map((l) => l.replace(/^\/\/ ?/, '')).join('\n'));
  } else await start();
} finally {
  rl?.close();
}
