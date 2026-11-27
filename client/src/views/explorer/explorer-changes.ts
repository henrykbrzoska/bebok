/**
 * Git gutter helpers: parse unified diff `@@` hunks (unified=0) into
 * per-line change sets for the explorer gutter.
 */
import type { FileDiffHunk } from '../../core/engine.dtos';

export interface GitLineChanges {
  added: Set<number>;
  removed: Set<number>;
  modified: Set<number>;
}

export function emptyChanges(): GitLineChanges {
  return { added: new Set(), removed: new Set(), modified: new Set() };
}

/** Classify hunks into added/removed/modified new-file line numbers. */
export function hunksToLineChanges(hunks: readonly FileDiffHunk[]): GitLineChanges {
  const out = emptyChanges();
  for (const h of hunks) {
    const ns = h.newStart ?? 0;
    const nl = h.newLines ?? 0;
    const ol = h.oldLines ?? 0;
    if (nl <= 0) {
      // Pure deletion: mark the anchor line (previous new line, min 1).
      out.removed.add(Math.max(1, ns));
      continue;
    }
    for (let i = 0; i < nl; i++) {
      const line = ns + i;
      if (ol <= 0) out.added.add(line);
      else out.modified.add(line);
    }
  }
  return out;
}

const HUNK_RE = /^@@\s+-(\d+)(?:,(\d+))?\s+\+(\d+)(?:,(\d+))?\s+@@/;

/** Parse a unified diff string into hunks (unified=0 style headers). */
export function parseDiffHunks(diff: string): FileDiffHunk[] {
  const hunks: FileDiffHunk[] = [];
  if (!diff) return hunks;
  for (const line of diff.split('\n')) {
    const m = HUNK_RE.exec(line.trim());
    if (!m) continue;
    hunks.push({
      oldStart: Number(m[1]),
      oldLines: m[2] === undefined ? 1 : Number(m[2]),
      newStart: Number(m[3]),
      newLines: m[4] === undefined ? 1 : Number(m[4]),
    });
  }
  return hunks;
}

/** Convenience: diff text -> line change sets. */
export function diffToLineChanges(diff: string): GitLineChanges {
  return hunksToLineChanges(parseDiffHunks(diff));
}

/** CSS class for a 1-based new-file line number. */
export function gutterClassFor(ch: GitLineChanges, line: number): string {
  if (ch.added.has(line)) return 'gutter-add';
  if (ch.removed.has(line)) return 'gutter-del';
  if (ch.modified.has(line)) return 'gutter-mod';
  return '';
}
