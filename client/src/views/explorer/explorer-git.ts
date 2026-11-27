/**
 * Pure helpers behind the Explorer's History / Blame column.
 *
 * The engine exposes `git log` (with a `path` filter) and `git show` (one
 * commit + its unified diff) but neither `git blame` nor "file at revision",
 * so blame and "compare with the working copy" are derived on the client from
 * the per-commit diffs: a commit's diff tells us which *new-side* line numbers
 * it added, which is exactly the attribution blame needs (newest commit wins),
 * and reverse-applying hunks walks a file back to its older content.
 */

/** One `@@` hunk of a single file's diff. */
export interface DiffHunk {
  /** First new-side line number of the hunk (1-based). */
  newStart: number;
  /** New-side line count. */
  newCount: number;
  /** Raw body lines, each keeping its ` `, `+` or `-` prefix. */
  lines: string[];
}

/** The part of one commit's diff that touches a single file. */
export interface FileDiff {
  /** New-side line numbers (1-based) this commit added for the file. */
  added: number[];
  hunks: DiffHunk[];
  /** False when the diff has no section for the file at all. */
  found: boolean;
}

function normalise(path: string): string {
  return path.replace(/\\/g, '/').replace(/^\.\//, '').replace(/^\/+/, '');
}

/**
 * Keep only the file's sections of a multi-file `git show` diff and report
 * the line numbers it added. Renames are matched on both sides of the
 * `diff --git a/… b/…` header.
 */
export function parseFileDiff(diff: string, path: string): FileDiff {
  const target = normalise(path);
  const added: number[] = [];
  const hunks: DiffHunk[] = [];
  let inFile = false;
  let found = false;
  let current: DiffHunk | null = null;
  let newLine = 0;

  const closeHunk = () => {
    if (current) {
      hunks.push(current);
      current = null;
    }
  };

  for (const raw of diff.split('\n')) {
    if (raw.startsWith('diff --git ')) {
      closeHunk();
      const match = /^diff --git (?:a\/)?(.+?) b\/(.+)$/.exec(raw);
      const paths = match ? [normalise(match[1]), normalise(match[2])] : [];
      inFile = paths.includes(target);
      found = found || inFile;
      newLine = 0;
      continue;
    }
    if (!inFile) {
      continue;
    }
    if (raw.startsWith('@@')) {
      closeHunk();
      const match = /^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@/.exec(raw);
      if (!match) {
        continue;
      }
      newLine = Number(match[1]);
      current = {
        newStart: newLine,
        newCount: match[2] === undefined ? 1 : Number(match[2]),
        lines: [],
      };
      continue;
    }
    if (!current) {
      continue; // file metadata (`index`, `---`, `+++`, mode lines)
    }
    current.lines.push(raw);
    if (raw.startsWith('+')) {
      added.push(newLine);
      newLine++;
    } else if (raw.startsWith(' ') || raw === '') {
      newLine++;
    }
  }
  closeHunk();
  return { added, hunks, found };
}

/** Split a hunk body into its old side and its new side. */
function hunkSides(hunk: DiffHunk): { old: string[]; new: string[] } {
  const oldLines: string[] = [];
  const newLines: string[] = [];
  for (const line of hunk.lines) {
    if (line.startsWith('-')) {
      oldLines.push(line.slice(1));
    } else if (line.startsWith('+')) {
      newLines.push(line.slice(1));
    } else {
      const text = line.startsWith(' ') ? line.slice(1) : line;
      oldLines.push(text);
      newLines.push(text);
    }
  }
  return { old: oldLines, new: newLines };
}

/**
 * Reverse-apply the file's hunks: the result is the file content *before*
 * the commit. Hunks whose context no longer matches (the working copy has
 * moved on) are skipped - the operation is best-effort by design.
 */
export function reverseApply(text: string, hunks: readonly DiffHunk[]): string {
  if (hunks.length === 0) {
    return text;
  }
  const lines = text.split('\n');
  for (let i = hunks.length - 1; i >= 0; i--) {
    const hunk = hunks[i];
    const { old: oldSide, new: newSide } = hunkSides(hunk);
    const start = Math.max(0, hunk.newStart - 1);
    const region = lines.slice(start, start + newSide.length);
    const matches =
      region.length === newSide.length && region.every((line, k) => line === newSide[k]);
    if (!matches) {
      continue;
    }
    lines.splice(start, newSide.length, ...oldSide);
  }
  return lines.join('\n');
}

type DiffOp = { kind: 'same' | 'add' | 'del'; text: string };

/** Above this many DP cells the middle block is reported as replaced. */
const LCS_CELL_CAP = 4_000_000;

/** Line diff of two texts (common prefix/suffix trimmed, LCS in between). */
function lineDiff(before: string, after: string): DiffOp[] {
  const a = before.split('\n');
  const b = after.split('\n');
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) {
    head++;
  }
  let tail = 0;
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) {
    tail++;
  }
  const midA = a.slice(head, a.length - tail);
  const midB = b.slice(head, b.length - tail);
  const ops: DiffOp[] = [];
  for (let i = 0; i < head; i++) {
    ops.push({ kind: 'same', text: a[i] });
  }
  const n = midA.length;
  const m = midB.length;
  if (n === 0 || m === 0 || n * m > LCS_CELL_CAP) {
    for (const text of midA) {
      ops.push({ kind: 'del', text });
    }
    for (const text of midB) {
      ops.push({ kind: 'add', text });
    }
  } else {
    const width = m + 1;
    const table = new Uint32Array((n + 1) * width);
    for (let i = n - 1; i >= 0; i--) {
      for (let j = m - 1; j >= 0; j--) {
        table[i * width + j] =
          midA[i] === midB[j]
            ? table[(i + 1) * width + j + 1] + 1
            : Math.max(table[(i + 1) * width + j], table[i * width + j + 1]);
      }
    }
    let i = 0;
    let j = 0;
    while (i < n && j < m) {
      if (midA[i] === midB[j]) {
        ops.push({ kind: 'same', text: midA[i] });
        i++;
        j++;
      } else if (table[(i + 1) * width + j] >= table[i * width + j + 1]) {
        ops.push({ kind: 'del', text: midA[i] });
        i++;
      } else {
        ops.push({ kind: 'add', text: midB[j] });
        j++;
      }
    }
    while (i < n) {
      ops.push({ kind: 'del', text: midA[i++] });
    }
    while (j < m) {
      ops.push({ kind: 'add', text: midB[j++] });
    }
  }
  for (let k = a.length - tail; k < a.length; k++) {
    ops.push({ kind: 'same', text: a[k] });
  }
  return ops;
}

/**
 * Unified diff between two texts, in the exact shape `app-diff-view` renders
 * (`--- a/…`, `+++ b/…`, `@@` hunks). Empty string when they are identical.
 */
export function unifiedDiff(
  before: string,
  after: string,
  beforeLabel: string,
  afterLabel: string,
  context = 3,
): string {
  const ops = lineDiff(before, after);
  const include = new Array<boolean>(ops.length).fill(false);
  ops.forEach((op, i) => {
    if (op.kind === 'same') {
      return;
    }
    const from = Math.max(0, i - context);
    const to = Math.min(ops.length - 1, i + context);
    for (let k = from; k <= to; k++) {
      include[k] = true;
    }
  });
  if (!include.some(Boolean)) {
    return '';
  }
  const oldNo: number[] = [];
  const newNo: number[] = [];
  let oldLine = 1;
  let newLine = 1;
  for (const op of ops) {
    oldNo.push(oldLine);
    newNo.push(newLine);
    if (op.kind === 'same') {
      oldLine++;
      newLine++;
    } else if (op.kind === 'del') {
      oldLine++;
    } else {
      newLine++;
    }
  }
  const out: string[] = [`--- a/${beforeLabel}`, `+++ b/${afterLabel}`];
  let i = 0;
  while (i < ops.length) {
    if (!include[i]) {
      i++;
      continue;
    }
    let j = i;
    while (j < ops.length && include[j]) {
      j++;
    }
    const slice = ops.slice(i, j);
    const oldCount = slice.filter((op) => op.kind !== 'add').length;
    const newCount = slice.filter((op) => op.kind !== 'del').length;
    out.push(`@@ -${oldNo[i]},${oldCount} +${newNo[i]},${newCount} @@`);
    for (const op of slice) {
      out.push((op.kind === 'add' ? '+' : op.kind === 'del' ? '-' : ' ') + op.text);
    }
    i = j;
  }
  return out.join('\n');
}
