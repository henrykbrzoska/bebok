/**
 * Minimal from-scratch QR Code encoder (WP-M4 / F10-13).
 *
 * Searched the tree first: no QR renderer is vendored anywhere in `client/`
 * and none of the existing npm dependencies (`@angular/*`, `@tauri-apps/*`,
 * `@xterm/*`, `highlight.js`) draw one, so per the brief this is written
 * in-tree instead of adding a new npm dependency for one small SVG.
 *
 * Byte mode only, error-correction level L, versions 1-10 (auto-selected by
 * the shortest that fits), a single fixed mask pattern (0: `(row+col) % 2
 * === 0`). That covers the pairing URI (~130 bytes worst case: a couple of
 * `http://100.x.y.z:8790` endpoints + an 8-char code + an 8-char
 * fingerprint) comfortably within version 10's ~271-byte capacity, without
 * the extra complexity of adaptive mask-penalty scoring or kanji/numeric
 * modes this app never needs. `encodeQr` returns `null` when the text does
 * not fit in version <= 10 - the caller falls back to the raw code as
 * selectable text, which is shown either way.
 *
 * Follows the public QR Code algorithm (ISO/IEC 18004): Reed-Solomon error
 * correction over GF(256), finder/timing/alignment function patterns, the
 * standard zigzag data placement, and format/version info blocks via BCH
 * codes. Not scanned against a physical camera as part of this package (see
 * the WP-M4 report) - the device test only opens `/remote/status` directly.
 */

const GF_EXP = new Uint8Array(512);
const GF_LOG = new Uint8Array(256);
(() => {
  let x = 1;
  for (let i = 0; i < 255; i++) {
    GF_EXP[i] = x;
    GF_LOG[x] = i;
    x <<= 1;
    if (x & 0x100) {
      x ^= 0x11d;
    }
  }
  for (let i = 255; i < 512; i++) {
    GF_EXP[i] = GF_EXP[i - 255];
  }
})();

function gfMul(a: number, b: number): number {
  if (a === 0 || b === 0) {
    return 0;
  }
  return GF_EXP[GF_LOG[a] + GF_LOG[b]];
}

function polyMul(a: number[], b: number[]): number[] {
  const res = new Array(a.length + b.length - 1).fill(0);
  for (let i = 0; i < a.length; i++) {
    for (let j = 0; j < b.length; j++) {
      res[i + j] ^= gfMul(a[i], b[j]);
    }
  }
  return res;
}

function rsGenerator(degree: number): number[] {
  let poly = [1];
  for (let i = 0; i < degree; i++) {
    poly = polyMul(poly, [1, GF_EXP[i]]);
  }
  return poly;
}

/** Reed-Solomon remainder (the error-correction codewords) for one block. */
function rsRemainder(data: number[], eccLen: number): number[] {
  const gen = rsGenerator(eccLen);
  const res = data.concat(new Array(eccLen).fill(0));
  for (let i = 0; i < data.length; i++) {
    const factor = res[i];
    if (factor === 0) {
      continue;
    }
    for (let j = 0; j < gen.length; j++) {
      res[i + j] ^= gfMul(gen[j], factor);
    }
  }
  return res.slice(data.length);
}

/** Per-version (1-10) EC level L layout: ecc bytes/block + block groups. */
const QR_LEVEL_L: Record<number, { ecc: number; groups: Array<[count: number, dataLen: number]> }> =
  {
    1: { ecc: 7, groups: [[1, 19]] },
    2: { ecc: 10, groups: [[1, 34]] },
    3: { ecc: 15, groups: [[1, 55]] },
    4: { ecc: 20, groups: [[1, 80]] },
    5: { ecc: 26, groups: [[1, 108]] },
    6: { ecc: 18, groups: [[2, 68]] },
    7: { ecc: 20, groups: [[2, 78]] },
    8: { ecc: 24, groups: [[2, 97]] },
    9: { ecc: 30, groups: [[2, 116]] },
    10: {
      ecc: 18,
      groups: [
        [2, 68],
        [2, 69],
      ],
    },
  };

/** Alignment-pattern coordinate list per version (2-10); version 1 has none. */
const ALIGNMENT: Record<number, number[]> = {
  2: [6, 18],
  3: [6, 22],
  4: [6, 26],
  5: [6, 30],
  6: [6, 34],
  7: [6, 22, 38],
  8: [6, 24, 42],
  9: [6, 26, 46],
  10: [6, 28, 50],
};

/** Remainder bits appended after interleaving, before placement (versions 1-10). */
const REMAINDER_BITS: Record<number, number> = {
  1: 0,
  2: 7,
  3: 7,
  4: 7,
  5: 7,
  6: 7,
  7: 0,
  8: 0,
  9: 0,
  10: 0,
};

function capacityBytes(version: number): number {
  return QR_LEVEL_L[version].groups.reduce((sum, [count, len]) => sum + count * len, 0);
}

function lengthBits(version: number): number {
  return version <= 9 ? 8 : 16;
}

/** Encode `text` (UTF-8) as a QR module matrix (`true` = dark), or `null` if it does not fit in version <= 10. */
export function encodeQr(text: string): boolean[][] | null {
  const data = Array.from(new TextEncoder().encode(text));

  let version = 0;
  for (let v = 1; v <= 10; v++) {
    const headerBits = 4 + lengthBits(v);
    const neededBytes = Math.ceil((headerBits + data.length * 8) / 8);
    if (neededBytes <= capacityBytes(v)) {
      version = v;
      break;
    }
  }
  if (version === 0) {
    return null;
  }

  // --- bit stream: mode + length + data + terminator + byte padding -------
  const bits: number[] = [];
  const pushBits = (value: number, len: number) => {
    for (let i = len - 1; i >= 0; i--) {
      bits.push((value >>> i) & 1);
    }
  };
  pushBits(0b0100, 4); // byte mode indicator
  pushBits(data.length, lengthBits(version));
  for (const byte of data) {
    pushBits(byte, 8);
  }
  const capacityInBits = capacityBytes(version) * 8;
  for (let i = 0; i < 4 && bits.length < capacityInBits; i++) {
    bits.push(0);
  }
  while (bits.length % 8 !== 0) {
    bits.push(0);
  }
  const padBytes = [0xec, 0x11];
  let padIndex = 0;
  while (bits.length < capacityInBits) {
    pushBits(padBytes[padIndex % 2], 8);
    padIndex++;
  }
  const codewords: number[] = [];
  for (let i = 0; i < bits.length; i += 8) {
    let byte = 0;
    for (let j = 0; j < 8; j++) {
      byte = (byte << 1) | bits[i + j];
    }
    codewords.push(byte);
  }

  // --- split into blocks, compute the Reed-Solomon ECC per block ----------
  const table = QR_LEVEL_L[version];
  const blocks: number[][] = [];
  let offset = 0;
  for (const [count, len] of table.groups) {
    for (let b = 0; b < count; b++) {
      blocks.push(codewords.slice(offset, offset + len));
      offset += len;
    }
  }
  const eccBlocks = blocks.map((block) => rsRemainder(block, table.ecc));

  // --- interleave data codewords, then ecc codewords -----------------------
  const finalCodewords: number[] = [];
  const maxDataLen = Math.max(...blocks.map((b) => b.length));
  for (let i = 0; i < maxDataLen; i++) {
    for (const block of blocks) {
      if (i < block.length) {
        finalCodewords.push(block[i]);
      }
    }
  }
  for (let i = 0; i < table.ecc; i++) {
    for (const block of eccBlocks) {
      finalCodewords.push(block[i]);
    }
  }
  const finalBits: number[] = [];
  for (const byte of finalCodewords) {
    for (let i = 7; i >= 0; i--) {
      finalBits.push((byte >> i) & 1);
    }
  }
  for (let i = 0; i < REMAINDER_BITS[version]; i++) {
    finalBits.push(0);
  }

  return buildMatrix(version, finalBits);
}

function buildMatrix(version: number, dataBits: number[]): boolean[][] {
  const size = version * 4 + 17;
  const matrix: boolean[][] = Array.from({ length: size }, () => new Array(size).fill(false));
  const isFn: boolean[][] = Array.from({ length: size }, () => new Array(size).fill(false));

  const drawFinder = (topRow: number, leftCol: number) => {
    for (let dy = -1; dy <= 7; dy++) {
      for (let dx = -1; dx <= 7; dx++) {
        const y = topRow + dy;
        const x = leftCol + dx;
        if (y < 0 || y >= size || x < 0 || x >= size) {
          continue;
        }
        isFn[y][x] = true;
        const inRing =
          dx >= 0 &&
          dx <= 6 &&
          dy >= 0 &&
          dy <= 6 &&
          (dx === 0 ||
            dx === 6 ||
            dy === 0 ||
            dy === 6 ||
            (dx >= 2 && dx <= 4 && dy >= 2 && dy <= 4));
        matrix[y][x] = inRing;
      }
    }
  };
  drawFinder(0, 0);
  drawFinder(0, size - 7);
  drawFinder(size - 7, 0);

  // timing patterns
  for (let i = 8; i <= size - 9; i++) {
    isFn[6][i] = true;
    matrix[6][i] = i % 2 === 0;
    isFn[i][6] = true;
    matrix[i][6] = i % 2 === 0;
  }

  // alignment patterns (skip the three combos that coincide with a finder)
  const coords = ALIGNMENT[version] ?? [];
  for (const r of coords) {
    for (const c of coords) {
      const nearFinder =
        (r === coords[0] && c === coords[0]) ||
        (r === coords[0] && c === coords[coords.length - 1]) ||
        (r === coords[coords.length - 1] && c === coords[0]);
      if (nearFinder) {
        continue;
      }
      for (let dy = -2; dy <= 2; dy++) {
        for (let dx = -2; dx <= 2; dx++) {
          const y = r + dy;
          const x = c + dx;
          isFn[y][x] = true;
          matrix[y][x] = Math.max(Math.abs(dx), Math.abs(dy)) !== 1;
        }
      }
    }
  }

  // the fixed dark module
  isFn[4 * version + 9][8] = true;
  matrix[4 * version + 9][8] = true;

  // reserve format-info areas (values are written after data placement)
  for (let i = 0; i <= 8; i++) {
    isFn[8][i] = true;
    isFn[i][8] = true;
  }
  for (let i = 0; i < 8; i++) {
    isFn[8][size - 1 - i] = true;
    isFn[size - 1 - i][8] = true;
  }

  // reserve version-info areas (versions 7-40 only)
  if (version >= 7) {
    for (let i = 0; i < 18; i++) {
      const a = Math.floor(i / 3);
      const b = i % 3;
      isFn[size - 11 + b][a] = true;
      isFn[a][size - 11 + b] = true;
    }
  }

  // place data bits in the standard zigzag order, applying mask 0 as we go
  let bitIndex = 0;
  for (let right = size - 1; right >= 1; right -= 2) {
    if (right === 6) {
      right = 5;
    }
    for (let vert = 0; vert < size; vert++) {
      for (let j = 0; j < 2; j++) {
        const x = right - j;
        const upward = ((right + 1) & 2) === 0;
        const y = upward ? size - 1 - vert : vert;
        if (isFn[y][x]) {
          continue;
        }
        const bit = bitIndex < dataBits.length ? dataBits[bitIndex] : 0;
        bitIndex++;
        const invert = (y + x) % 2 === 0; // mask pattern 0
        matrix[y][x] = (bit ^ (invert ? 1 : 0)) === 1;
      }
    }
  }

  // format info: EC level L (0b01) + mask 0, BCH(15,5), xor 0x5412
  const formatData = (0b01 << 3) | 0;
  let frem = formatData;
  for (let i = 0; i < 10; i++) {
    frem = (frem << 1) ^ ((frem >>> 9) * 0x537);
  }
  frem &= 0x3ff;
  const formatBits = ((formatData << 10) | frem) ^ 0x5412;
  const fbit = (i: number) => (formatBits >>> i) & 1;

  // first copy: bits 0-7 down column 8 (skipping the timing row), 8-14 along row 8 leftwards
  for (let i = 0; i <= 5; i++) {
    matrix[i][8] = fbit(i) === 1;
  }
  matrix[7][8] = fbit(6) === 1;
  matrix[8][8] = fbit(7) === 1;
  matrix[8][7] = fbit(8) === 1;
  for (let i = 9; i <= 14; i++) {
    matrix[8][14 - i] = fbit(i) === 1;
  }
  // second copy: bits 0-7 along row 8 from the right edge, 8-14 down column 8 at the bottom
  for (let i = 0; i <= 7; i++) {
    matrix[8][size - 1 - i] = fbit(i) === 1;
  }
  for (let i = 8; i <= 14; i++) {
    matrix[size - 15 + i][8] = fbit(i) === 1;
  }

  // version info (versions 7-40 only): BCH(18,6), no xor mask
  if (version >= 7) {
    let vrem = version;
    for (let i = 0; i < 12; i++) {
      vrem = (vrem << 1) ^ ((vrem >>> 11) * 0x1f25);
    }
    vrem &= 0xfff;
    const versionBits = (version << 12) | vrem;
    const vbit = (i: number) => (versionBits >>> i) & 1;
    for (let i = 0; i < 18; i++) {
      const a = Math.floor(i / 3);
      const b = i % 3;
      matrix[size - 11 + b][a] = vbit(i) === 1;
      matrix[a][size - 11 + b] = vbit(i) === 1;
    }
  }

  return matrix;
}

/** Render a module matrix as an inline SVG string (currentColor fill, no background rect). */
export function qrToSvg(matrix: boolean[][], moduleSize = 4): string {
  const size = matrix.length;
  const px = size * moduleSize;
  let path = '';
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      if (matrix[y][x]) {
        path += `M${x * moduleSize},${y * moduleSize}h${moduleSize}v${moduleSize}h${-moduleSize}z`;
      }
    }
  }
  return (
    `<svg viewBox="0 0 ${px} ${px}" width="100%" height="100%" style="display:block" ` +
    `xmlns="http://www.w3.org/2000/svg" role="img" shape-rendering="crispEdges">` +
    `<path d="${path}" fill="currentColor"/></svg>`
  );
}
