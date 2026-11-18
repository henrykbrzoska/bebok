/**
 * IntelliJ-style fuzzy matching for the Explorer "Go to" dialog
 * (Ctrl/Cmd+Shift+N files, Ctrl/Cmd+N classes, Ctrl/Cmd+Alt+Shift+N symbols).
 *
 * - Pattern characters must appear in order (case-insensitive); word starts
 *   (`FindInFiles` -> `fif`, `find-in-files.ts` -> `fif`), consecutive runs
 *   and prefixes score higher, gaps cost a little.
 * - Spaces and `*` are wildcards (just dropped: the match is already a
 *   subsequence).
 * - `dir/name` patterns: the part after the last `/` must match the file
 *   name, the part before it the directory path.
 * - A trailing `:line[:column]` is split off and returned separately.
 */

export interface FuzzyMatch {
  score: number;
  /** Matched character positions in the target (for highlighting). */
  indices: number[];
}

export interface GotoQuery {
  /** Pattern without wildcards / line suffix. */
  pattern: string;
  line?: number;
  column?: number;
}

export interface FileMatch {
  path: string;
  score: number;
  /** Matched positions inside the file name. */
  nameIndices: number[];
  /** Matched positions inside the directory part. */
  dirIndices: number[];
}

const NEG = -1e9;
const BONUS_BOUNDARY = 8;
const BONUS_CONSECUTIVE = 5;
const BONUS_CASE = 1;
const GAP = 1;
const LEADING_GAP_MAX = 6;

/** Split `foo.ts:12:3` into the pattern and the location. */
export function parseGotoQuery(raw: string): GotoQuery {
  let text = raw.trim();
  let line: number | undefined;
  let column: number | undefined;
  const m = /:(\d+)(?::(\d+))?$/.exec(text);
  if (m) {
    line = Number(m[1]);
    column = m[2] !== undefined ? Number(m[2]) : undefined;
    text = text.slice(0, m.index);
  }
  return { pattern: text.replace(/[\s*]+/g, '').replace(/\\/g, '/'), line, column };
}

function isBoundary(target: string, j: number): boolean {
  if (j === 0) {
    return true;
  }
  const prev = target[j - 1];
  const cur = target[j];
  if (/[/\\._\-\s$:]/.test(prev)) {
    return true;
  }
  // camelCase hump / letter -> digit
  if (cur >= 'A' && cur <= 'Z' && !(prev >= 'A' && prev <= 'Z')) {
    return true;
  }
  return /\d/.test(cur) && !/\d/.test(prev);
}

/**
 * Best-scoring subsequence alignment of `pattern` inside `target` (DP,
 * O(pattern × target)), or `null` when `pattern` is not a subsequence.
 */
export function fuzzyMatch(pattern: string, target: string): FuzzyMatch | null {
  const m = pattern.length;
  const n = target.length;
  if (m === 0) {
    return { score: 0, indices: [] };
  }
  if (m > n) {
    return null;
  }
  const p = pattern.toLowerCase();
  const t = target.toLowerCase();
  // Cheap subsequence check first - most candidates fail here.
  for (let i = 0, j = 0; i < m; j++) {
    if (j >= n) {
      return null;
    }
    if (t[j] === p[i]) {
      i++;
    }
  }

  const score = new Float64Array(m * n).fill(NEG);
  const parent = new Int32Array(m * n).fill(-1);
  for (let i = 0; i < m; i++) {
    let gap = NEG;
    let gapFrom = -1;
    for (let j = 0; j < n; j++) {
      if (i > 0 && j >= 1) {
        // `gap` = best previous-row cell at k <= j-2, penalised by distance.
        // Before using it at j, fold in k = j-2.
        if (j >= 2) {
          const cand = score[(i - 1) * n + (j - 2)] - GAP;
          if (cand > gap - GAP) {
            gap = cand;
            gapFrom = j - 2;
          } else {
            gap -= GAP;
          }
        }
      }
      if (t[j] !== p[i]) {
        continue;
      }
      let base: number;
      let from = -1;
      if (i === 0) {
        base = -Math.min(j, LEADING_GAP_MAX) * 0.5;
      } else {
        const adj = j >= 1 ? score[(i - 1) * n + (j - 1)] : NEG;
        const viaAdj = adj > NEG / 2 ? adj + BONUS_CONSECUTIVE : NEG;
        if (viaAdj >= gap) {
          base = viaAdj;
          from = j - 1;
        } else {
          base = gap;
          from = gapFrom;
        }
        if (base <= NEG / 2) {
          continue;
        }
      }
      let bonus = 1;
      if (isBoundary(target, j)) {
        bonus += BONUS_BOUNDARY;
      }
      if (pattern[i] === target[j] && pattern[i] !== p[i]) {
        bonus += BONUS_CASE; // an uppercase pattern char hit an uppercase hump
      }
      score[i * n + j] = base + bonus;
      parent[i * n + j] = from;
    }
  }

  let best = NEG;
  let bestJ = -1;
  for (let j = 0; j < n; j++) {
    const s = score[(m - 1) * n + j];
    if (s > best) {
      best = s;
      bestJ = j;
    }
  }
  if (bestJ < 0 || best <= NEG / 2) {
    return null;
  }
  const indices = new Array<number>(m);
  for (let i = m - 1, j = bestJ; i >= 0; i--) {
    indices[i] = j;
    j = parent[i * n + j];
  }
  let total = best;
  if (t.startsWith(p)) {
    total += 10;
  }
  return { score: total, indices };
}

function splitPath(path: string): { dir: string; name: string } {
  const i = path.lastIndexOf('/');
  return i < 0 ? { dir: '', name: path } : { dir: path.slice(0, i), name: path.slice(i + 1) };
}

function stem(name: string): string {
  const i = name.lastIndexOf('.');
  return i > 0 ? name.slice(0, i) : name;
}

/** Score one file path against a parsed pattern, `null` when it misses. */
export function matchFile(pattern: string, path: string): FileMatch | null {
  const { dir, name } = splitPath(path);
  if (!pattern) {
    return { path, score: 0, nameIndices: [], dirIndices: [] };
  }
  const slash = pattern.lastIndexOf('/');
  if (slash >= 0) {
    const dirPat = pattern.slice(0, slash).replace(/^\/+/, '');
    const namePat = pattern.slice(slash + 1);
    const nm = fuzzyMatch(namePat, name);
    if (!nm) {
      return null;
    }
    const dm = dirPat ? matchDirSegments(dirPat, dir) : { score: 0, indices: [] };
    if (!dm) {
      return null;
    }
    return {
      path,
      score: nm.score + dm.score * 0.5 + exactBonus(namePat, name),
      nameIndices: nm.indices,
      dirIndices: dm.indices,
    };
  }
  const nm = fuzzyMatch(pattern, name);
  if (nm) {
    return {
      path,
      score: nm.score + exactBonus(pattern, name),
      nameIndices: nm.indices,
      dirIndices: [],
    };
  }
  // Fallback: the pattern spans directory + name (`explorerhtml`).
  if (pattern.length < 3) {
    return null;
  }
  const full = fuzzyMatch(pattern, path);
  if (!full) {
    return null;
  }
  const cut = dir ? dir.length + 1 : 0;
  return {
    path,
    score: full.score - 25,
    nameIndices: full.indices.filter((i) => i >= cut).map((i) => i - cut),
    dirIndices: full.indices.filter((i) => i < dir.length),
  };
}

/**
 * Each `/`-separated piece of `pattern` must match inside one directory
 * segment of `dir`, in order (segments may be skipped): `vi/expl` matches
 * `client/src/views/explorer`, `core` does not match `client/src/views`.
 */
function matchDirSegments(pattern: string, dir: string): FuzzyMatch | null {
  const pieces = pattern.split('/').filter((p) => p.length > 0);
  const segs = dir.split('/');
  const offsets: number[] = [];
  let off = 0;
  for (const s of segs) {
    offsets.push(off);
    off += s.length + 1;
  }
  let score = 0;
  const indices: number[] = [];
  let k = 0;
  for (const piece of pieces) {
    let found: FuzzyMatch | null = null;
    while (k < segs.length && !(found = fuzzyMatch(piece, segs[k]))) {
      k++;
    }
    if (!found) {
      return null;
    }
    score += found.score;
    indices.push(...found.indices.map((i) => i + offsets[k]));
    k++;
  }
  return { score, indices };
}

function exactBonus(pattern: string, name: string): number {
  const p = pattern.toLowerCase();
  const n = name.toLowerCase();
  if (n === p) {
    return 30;
  }
  return stem(n) === p ? 20 : 0;
}

/** Rank `files` against `pattern`; best first, at most `limit`. */
export function rankFiles(files: readonly string[], pattern: string, limit: number): FileMatch[] {
  const out: FileMatch[] = [];
  for (const f of files) {
    const m = matchFile(pattern, f);
    if (m) {
      out.push(m);
    }
  }
  out.sort(
    (a, b) => b.score - a.score || a.path.length - b.path.length || a.path.localeCompare(b.path),
  );
  return out.slice(0, limit);
}

/** Tree-sitter node kinds that count as "classes" (types) for Ctrl/Cmd+N. */
export function isClassKind(kind: string): boolean {
  return /class|struct|interface|trait|enum_(item|declaration)|union|type_(item|alias|spec)|type_alias_declaration/.test(
    kind,
  );
}

/** Short human label for a tree-sitter kind (`function_item` -> `function`). */
export function kindLabel(kind: string): string {
  const k = kind
    .replace(/_(item|declaration|definition|declarator|spec|signature|element|elem)$/, '')
    .replace(/^(abstract_|generator_|public_|internal_)/, '');
  const map: Record<string, string> = {
    function: 'fn',
    method: 'method',
    class: 'class',
    struct: 'struct',
    interface: 'interface',
    trait: 'trait',
    enum: 'enum',
    enum_variant: 'variant',
    enum_case: 'case',
    type: 'type',
    type_alias: 'type',
    mod: 'mod',
    module: 'module',
    const: 'const',
    static: 'static',
    field: 'field',
    property: 'prop',
    variable: 'var',
    var: 'var',
    lexical: 'var',
    macro: 'macro',
  };
  return map[k] ?? k.replace(/_/g, ' ');
}

/** Split `text` into `{ text, hit }` runs for highlighting `indices`. */
export function highlightRuns(
  text: string,
  indices: readonly number[],
): { text: string; hit: boolean }[] {
  if (indices.length === 0) {
    return [{ text, hit: false }];
  }
  const set = new Set(indices);
  const out: { text: string; hit: boolean }[] = [];
  for (let i = 0; i < text.length; i++) {
    const hit = set.has(i);
    const last = out[out.length - 1];
    if (last && last.hit === hit) {
      last.text += text[i];
    } else {
      out.push({ text: text[i], hit });
    }
  }
  return out;
}
