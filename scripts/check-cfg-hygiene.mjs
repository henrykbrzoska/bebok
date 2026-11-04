#!/usr/bin/env node
// check-cfg-hygiene.mjs — catch "compiles on Linux, fails on Windows (or the
// reverse)" before pushing.
//
// The failure this exists for:
//
//     error: unused import: `Instant`
//     error: constant `TERM_GRACE` is never used
//
// Both were used only inside a `#[cfg(not(windows))]` branch. On Linux they are
// live, so clippy is silent; on Windows the branch is compiled out and
// `-D warnings` fails the build. Nobody runs Windows locally, so the first
// signal is a red CI run minutes after the push.
//
// Rule enforced here: a module-level `use` / `const` / `static` whose every
// reference sits inside a `#[cfg(...)]`-gated region is dead code on the other
// platform. Declare it inside the gated branch instead — or, better, move the
// branch into its own `#[cfg(...)] mod`, which is the pattern the CLI runner
// uses for its process table.
//
//   node scripts/check-cfg-hygiene.mjs [path]     (default: engine/crates)
//
// Exits non-zero and prints file:line for every violation.

import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(fileURLToPath(new URL('..', import.meta.url)));
const target = process.argv[2] ? join(ROOT, process.argv[2]) : join(ROOT, 'engine', 'crates');

const SKIP_DIRS = new Set(['target', 'node_modules', '.git', 'vendor']);

function* rustFiles(dir) {
  let entries;
  try {
    entries = readdirSync(dir);
  } catch {
    return;
  }
  for (const name of entries) {
    const full = join(dir, name);
    let st;
    try {
      st = statSync(full);
    } catch {
      continue;
    }
    if (st.isDirectory()) {
      if (!SKIP_DIRS.has(name)) yield* rustFiles(full);
    } else if (name.endsWith('.rs')) {
      yield full;
    }
  }
}

// ---------------------------------------------------------------------------
// Brace-aware line classification
// ---------------------------------------------------------------------------

/**
 * Mark the line ranges that a `#[cfg(...)]` attribute gates.
 *
 * An attribute applies to the item that follows it, so the gated range is that
 * item's body: up to its matching `}` for a braced item, up to the `;` for a
 * statement-like one.
 */
function cfgRegions(lines) {
  const regions = [];
  for (let i = 0; i < lines.length; i++) {
    if (!/#\[cfg(\(|_attr\()/.test(lines[i])) continue;
    // Collect the whole attribute, which may span lines.
    let attr = lines[i];
    let end = i;
    while (!/\)\]/.test(attr) && end + 1 < lines.length) attr += lines[++end];

    // `cfg(not(any(windows, target_os = "macos")))` also means "not on Windows",
    // so negation is detected on the outer `not(` rather than on `not(windows)`.
    const negated = /cfg\(\s*not\s*\(/.test(attr);
    const polarity = /\bwindows\b/.test(attr)
      ? negated
        ? 'not(windows)'
        : 'windows'
      : /\bunix\b/.test(attr)
        ? 'unix'
        : 'other';

    // Find where the gated item starts, then walk to its end.
    let depth = (attr.match(/\{/g) || []).length - (attr.match(/\}/g) || []).length;
    let j = end;
    let sawBrace = depth > 0;
    if (!sawBrace) {
      for (j = end + 1; j < lines.length; j++) {
        const line = lines[j];
        if (/^\s*(\/\/|$)/.test(line)) continue;
        break;
      }
    }
    // The gated range is the item body. `sawBrace` must latch as soon as a `{`
    // is seen, not only when depth stays positive: a one-line `fn f() { .. }`
    // opens and closes on the same line, and treating that as "no body yet"
    // let the region run to the end of the file.
    let k = j;
    for (; k < lines.length; k++) {
      const line = lines[k];
      if (line.includes('{')) sawBrace = true;
      depth += (line.match(/\{/g) || []).length;
      depth -= (line.match(/\}/g) || []).length;
      // `;` ends a statement-like item, `,` ends a struct/enum field.
      if (sawBrace ? depth <= 0 : /[;,](\s*\/\/.*)?$/.test(line)) break;
    }
    regions.push({ from: j, to: Math.min(k, lines.length - 1), polarity });
  }
  return regions;
}

function regionOf(regions, line) {
  let found = null;
  for (const r of regions) {
    if (line >= r.from && line <= r.to) found = r; // nested wins: keep the last
  }
  return found;
}

// ---------------------------------------------------------------------------
// Module-level declarations
// ---------------------------------------------------------------------------

const DECL = /^(?:pub(?:\([^)]*\))?\s+)?(?:use\s+([^;]+);|(?:const|static)\s+(?:mut\s+)?([A-Za-z_][A-Za-z0-9_]*))/;

function declaredNames(line) {
  const m = DECL.exec(line);
  if (!m) return [];
  if (m[1] !== undefined) {
    // `use a::b::{c, d as e}` / `use a::b;` — take the bound identifiers.
    const body = m[1].replace(/\s+as\s+/g, ' as ');
    const names = [];
    for (const raw of body.split(/[{},]/)) {
      const piece = raw.trim();
      if (!piece) continue;
      const asMatch = /(?:^|::)([A-Za-z_][A-Za-z0-9_]*)(?:\s+as\s+([A-Za-z_][A-Za-z0-9_]*))?$/.exec(piece);
      if (!asMatch) continue;
      names.push(asMatch[2] || asMatch[1]);
    }
    return names;
  }
  return [m[2]];
}

/**
 * Declarations that exist on one platform only, called from code that is not
 * gated at all — a hard "cannot find function" on the other platform, which is
 * worse than a warning.
 */
function missingElsewhere(lines, regions) {
  const out = [];
  const FN = /^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)/;

  // A name implemented once per platform (`#[cfg(windows)] fn f` next to
  // `#[cfg(not(windows))] fn f`) is complete, not missing anywhere.
  const definedIn = new Map();
  for (let i = 0; i < lines.length; i++) {
    const m = FN.exec(lines[i]);
    if (!m) continue;
    const r = regionOf(regions, i);
    if (!r) continue;
    if (!definedIn.has(m[1])) definedIn.set(m[1], new Set());
    definedIn.get(m[1]).add(r.polarity);
  }
  const coversBothPlatforms = (name) => {
    const p = definedIn.get(name);
    if (!p) return false;
    const unixish = p.has('unix') || p.has('not(windows)');
    return (p.has('windows') && unixish) || (p.has('unix') && p.has('not(windows)'));
  };
  for (let i = 0; i < lines.length; i++) {
    const m = FN.exec(lines[i]);
    if (!m) continue;
    const region = regionOf(regions, i);
    if (!region) continue; // ungated: exists everywhere
    if (region.polarity !== 'windows' && region.polarity !== 'unix' && region.polarity !== 'not(windows)') {
      continue; // #[cfg(test)] etc: the callers are gated too
    }
    const name = m[1];
    if (coversBothPlatforms(name)) continue;
    // No `g` flag: `.test()` on a global regex is stateful (lastIndex) and
    // silently skips every other match.
    const re = new RegExp(`\\b${name}\\s*\\(`);
    for (let j = 0; j < lines.length; j++) {
      if (j >= region.from - 1 && j <= region.to) continue; // its own body
      if (!re.test(lines[j])) continue;
      const refRegion = regionOf(regions, j);
      if (!refRegion) {
        out.push({ line: i + 1, name, only: region.polarity, text: lines[i].trim().slice(0, 100), refLine: j + 1 });
        break;
      }
    }
  }
  return out;
}

const violations = [];

for (const file of rustFiles(target)) {
  const src = readFileSync(file, 'utf8');
  if (!src.includes('cfg(')) continue;
  const lines = src.split(/\r?\n/);
  const regions = cfgRegions(lines);
  if (regions.length === 0) continue;

  const byPolarity = new Map();
  for (const r of regions) {
    if (!byPolarity.has(r.polarity)) byPolarity.set(r.polarity, []);
    byPolarity.get(r.polarity).push(r);
  }

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    if (!/^[A-Za-z_#/]/.test(line) || /^\s/.test(line)) continue; // module level only
    // A declaration that is itself gated is the correct pattern: it simply
    // does not exist on the other platform.
    if (regionOf(regions, i)) continue;

    for (const name of declaredNames(line)) {
      if (!name || name === 'self' || name === 'super' || name === 'crate') continue;
      const re = new RegExp(`\\b${name.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`, 'g');
      let refs = 0;
      const seenIn = new Set();
      for (let j = 0; j < lines.length; j++) {
        if (j === i) continue;
        const hits = (lines[j].match(re) || []).length;
        if (!hits) continue;
        refs += hits;
        const r = regionOf(regions, j);
        seenIn.add(r ? r.polarity : 'none');
      }
      if (refs === 0) continue; // truly unused: clippy already reports it
      if (seenIn.has('none')) continue; // used outside cfg: always live
      // Every reference lives in a cfg branch, and all of them are the same
      // platform -> dead on the complement.
      const platforms = new Set([...seenIn].filter((p) => p !== 'other'));
      if (platforms.size !== 1) continue;
      const only = [...platforms][0];
      if (only !== 'windows' && only !== 'unix' && only !== 'not(windows)') continue;
      violations.push({
        file,
        line: i + 1,
        name,
        only,
        text: line.trim().slice(0, 100),
      });
    }
  }

  for (const missing of missingElsewhere(lines, regions)) {
    violations.push({
      file,
      line: missing.line,
      name: missing.name,
      only: `${missing.only} only, called from ungated code`,
      text: `${missing.text}   <- called at line ${missing.refLine}`,
    });
  }
}

if (violations.length === 0) {
  console.log(`✓ cfg hygiene: no platform-only dead items in ${relative(ROOT, target) || '.'}`);
  process.exit(0);
}

console.error(
  `✗ cfg hygiene: ${violations.length} module-level item(s) referenced only inside a cfg branch.\n` +
    `  They are dead code on the other platform, and -D warnings then fails that build.\n` +
    `  Fix: declare the item inside the gated branch, or move the branch into its own\n` +
    `  #[cfg(...)] mod (see bebok-cli-agents/src/runner.rs "platform").\n`,
);
for (const v of violations) {
  console.error(`  ${relative(ROOT, v.file)}:${v.line}  [${v.only} only]  ${v.name}`);
  console.error(`      ${v.text}`);
}
process.exit(1);
