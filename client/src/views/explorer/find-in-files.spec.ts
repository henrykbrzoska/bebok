import { splitMatch, trimHitLine } from './find-in-files';

describe('find-in-files helpers', () => {
  it('splits a line around the match', () => {
    expect(splitMatch('let foo = 1;', 4, 3)).toEqual(['let ', 'foo', ' = 1;']);
  });

  it('counts characters, not UTF-16 units', () => {
    expect(splitMatch('😀 foo', 2, 3)).toEqual(['😀 ', 'foo', '']);
  });

  it('returns the whole text when there is no span', () => {
    expect(splitMatch('abc', null, 0)).toEqual(['abc', '', '']);
    expect(splitMatch('abc', 1, 0)).toEqual(['abc', '', '']);
  });

  it('trims indentation and shifts the match start', () => {
    expect(trimHitLine('    return foo;', 12)).toEqual({ text: 'return foo;', start: 7 });
    expect(trimHitLine('foo', 1)).toEqual({ text: 'foo', start: 0 });
  });
});
