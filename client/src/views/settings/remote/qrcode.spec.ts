import { encodeQr, qrToSvg } from './qrcode';

describe('encodeQr', () => {
  it('encodes a short string into a version-1-sized matrix (21x21)', () => {
    const matrix = encodeQr('hello');
    expect(matrix).not.toBeNull();
    expect(matrix!.length).toBe(21);
    expect(matrix!.every((row) => row.length === 21)).toBeTrue();
  });

  it('places the three finder patterns (dark corners)', () => {
    const matrix = encodeQr('bebok://pair?v=1&ep=http://100.101.102.103:8790&code=ABCDEFGH&fp=deadbeef')!;
    const size = matrix.length;
    // top-left finder centre is always dark
    expect(matrix[3][3]).toBeTrue();
    // top-right finder centre
    expect(matrix[3][size - 4]).toBeTrue();
    // bottom-left finder centre
    expect(matrix[size - 4][3]).toBeTrue();
    // the ring's second row is light between the dark border and dark centre
    expect(matrix[1][1]).toBeFalse();
  });

  it('grows the version as the payload grows', () => {
    const short = encodeQr('bebok://pair?v=1&code=ABCDEFGH')!;
    const long = encodeQr(
      'bebok://pair?v=1&ep=http://100.101.102.103:8790,http://100.101.102.104:8790&code=ABCDEFGH&fp=deadbeef',
    )!;
    expect(long.length).toBeGreaterThan(short.length);
  });

  it('returns null when the text cannot fit in version <= 10', () => {
    const huge = 'x'.repeat(2000);
    expect(encodeQr(huge)).toBeNull();
  });

  it('is deterministic for the same input', () => {
    const a = encodeQr('bebok://pair?v=1&code=ABCDEFGH');
    const b = encodeQr('bebok://pair?v=1&code=ABCDEFGH');
    expect(a).toEqual(b);
  });
});

describe('qrToSvg', () => {
  it('renders an svg whose viewBox matches the matrix size', () => {
    const matrix = encodeQr('hello')!;
    const svg = qrToSvg(matrix, 4);
    expect(svg).toContain(`viewBox="0 0 ${matrix.length * 4} ${matrix.length * 4}"`);
    expect(svg).toContain('<svg');
    expect(svg).toContain('fill="currentColor"');
  });
});
