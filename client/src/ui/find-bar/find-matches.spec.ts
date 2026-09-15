/**
 * Pure matching logic of the find bar: literal vs regex queries, case and
 * whole-word toggles, the result cap and the zero-length safety net.
 */

import { FindOptions } from '../../core/find.store';
import { buildFindRegex, escapeRegExp, findAll } from './find-matches';

const LITERAL: FindOptions = { matchCase: false, wholeWord: false, useRegex: false };

function regexOf(query: string, options: Partial<FindOptions> = {}): RegExp {
  const result = buildFindRegex(query, { ...LITERAL, ...options });
  if ('error' in result) {
    throw new Error('expected a regex');
  }
  return result.regex;
}

describe('buildFindRegex', () => {
  it('escapes the query when useRegex is off', () => {
    expect(escapeRegExp('a.b*c')).toBe('a\\.b\\*c');
    const hits = findAll('xx a.b xx axb xx', regexOf('a.b'));
    expect(hits.ranges.length).toBe(1);
    expect(hits.ranges[0]).toEqual({ start: 3, length: 3 });
  });

  it('uses the global flag, and the case-insensitive one by default', () => {
    expect(regexOf('cat').flags).toContain('g');
    expect(regexOf('cat').flags).toContain('i');
    expect(regexOf('cat', { matchCase: true }).flags).not.toContain('i');
  });

  it('matches case-insensitively, or exactly when matchCase is on', () => {
    const text = 'Cat dog cat';
    expect(findAll(text, regexOf('cat')).ranges.length).toBe(2);
    expect(findAll(text, regexOf('cat', { matchCase: true })).ranges.length).toBe(1);
  });

  it('honours wholeWord: cat does not hit category', () => {
    const text = 'cat category cat';
    expect(findAll(text, regexOf('cat')).ranges.length).toBe(3);
    const whole = findAll(text, regexOf('cat', { wholeWord: true }));
    expect(whole.ranges.length).toBe(2);
    expect(whole.ranges).toEqual([
      { start: 0, length: 3 },
      { start: 13, length: 3 },
    ]);
  });

  it('applies wholeWord to a regex query too', () => {
    const text = 'cat scat';
    expect(findAll(text, regexOf('c.t', { useRegex: true })).ranges.length).toBe(2);
    const whole = findAll(text, regexOf('c.t', { useRegex: true, wholeWord: true }));
    expect(whole.ranges.length).toBe(1);
  });

  it('reports an invalid pattern instead of throwing', () => {
    const result = buildFindRegex('(', { ...LITERAL, useRegex: true });
    expect('error' in result).toBeTrue();
    if ('error' in result) {
      expect(result.error).toBe('regex');
    }
  });
});

describe('findAll', () => {
  it('returns no ranges for an empty query', () => {
    expect(findAll('anything', regexOf('')).ranges).toEqual([]);
    expect(findAll('anything', regexOf('')).limited).toBeFalse();
    expect(findAll('anything', regexOf('', { wholeWord: true })).ranges).toEqual([]);
    expect(findAll('anything', regexOf('', { useRegex: true })).ranges).toEqual([]);
  });

  it('returns no ranges for empty text', () => {
    expect(findAll('', regexOf('a')).ranges).toEqual([]);
  });

  it('collects every match with its offset and length', () => {
    const result = findAll('ab ab ab', regexOf('ab'));
    expect(result.ranges).toEqual([
      { start: 0, length: 2 },
      { start: 3, length: 2 },
      { start: 6, length: 2 },
    ]);
    expect(result.limited).toBeFalse();
  });

  it('stops at the cap and flags the result as limited', () => {
    const text = 'a'.repeat(600);
    const capped = findAll(text, regexOf('a'), 500);
    expect(capped.ranges.length).toBe(500);
    expect(capped.limited).toBeTrue();
    const uncapped = findAll('a'.repeat(10), regexOf('a'), 500);
    expect(uncapped.ranges.length).toBe(10);
    expect(uncapped.limited).toBeFalse();
  });

  it('advances on zero-length matches instead of looping forever', () => {
    const result = findAll('aaa', /(?=a)/g);
    expect(result.ranges.length).toBe(3);
    expect(result.ranges.every((range) => range.length === 0)).toBeTrue();
    const star = findAll('baa', /a*/g);
    expect(star.ranges.length).toBeGreaterThan(0);
  });

  it('strips a sticky caller flag so the scan still walks the text', () => {
    expect(findAll('ab ab', /ab/).ranges.length).toBe(2);
    expect(findAll('ab ab', /ab/y).ranges.length).toBe(2);
  });
});
