/**
 * DOM helpers of the find bar: the `<mark class="find-hit">` fallback must be
 * reversible (text restored, nodes merged) and must never throw on a missing
 * API or an out-of-range slice.
 */

import {
  FIND_HIT_CLASS,
  supportsHighlightApi,
  unwrapMarks,
  wrapRangeWithMark,
  wrapRangesWithMarks,
} from './find-highlight';

/** Hosts created by a test; removed afterwards (never clear `document.body`,
 * the Jasmine HTML reporter lives there and would break on teardown). */
const mounted: HTMLElement[] = [];

function host(text: string): { root: HTMLElement; node: Text } {
  const root = document.createElement('div');
  const node = document.createTextNode(text);
  root.appendChild(node);
  document.body.appendChild(root);
  mounted.push(root);
  return { root, node };
}

describe('find-highlight', () => {
  afterEach(() => {
    while (mounted.length > 0) {
      mounted.pop()?.remove();
    }
  });

  it('reports whether the Custom Highlight API is usable without throwing', () => {
    expect(typeof supportsHighlightApi()).toBe('boolean');
  });

  it('wraps a range in a mark.find-hit element', () => {
    const { root, node } = host('hello world');
    wrapRangeWithMark(node, 6, 5);
    expect(root.textContent).toBe('hello world');
    const mark = root.querySelector(`mark.${FIND_HIT_CLASS}`) as HTMLElement | null;
    expect(mark).toBeTruthy();
    expect(mark?.textContent).toBe('world');
    expect(root.childNodes.length).toBe(2);
  });

  it('splits a range out of the middle of a text node', () => {
    const { root, node } = host('a needle b');
    wrapRangeWithMark(node, 2, 6);
    expect(root.childNodes.length).toBe(3);
    expect(root.querySelector(`mark.${FIND_HIT_CLASS}`)?.textContent).toBe('needle');
    expect(root.textContent).toBe('a needle b');
  });

  it('ignores an empty or out-of-range slice', () => {
    const { root, node } = host('abc');
    wrapRangeWithMark(node, 1, 0);
    wrapRangeWithMark(node, 2, 10);
    wrapRangeWithMark(node, -1, 2);
    expect(root.querySelector('mark')).toBeNull();
    expect(root.textContent).toBe('abc');
  });

  it('wrapRangesWithMarks handles several ranges of one node', () => {
    const { root, node } = host('ab ab ab');
    wrapRangesWithMarks(node, [
      { start: 0, length: 2 },
      { start: 3, length: 2 },
      { start: 6, length: 2 },
    ]);
    expect(root.querySelectorAll(`mark.${FIND_HIT_CLASS}`).length).toBe(3);
    expect(root.textContent).toBe('ab ab ab');
  });

  it('unwrapMarks restores the original text and merges the nodes', () => {
    const { root, node } = host('find me in here');
    wrapRangesWithMarks(node, [
      { start: 0, length: 4 },
      { start: 8, length: 2 },
    ]);
    expect(root.querySelectorAll(`mark.${FIND_HIT_CLASS}`).length).toBe(2);
    unwrapMarks(root);
    expect(root.querySelectorAll('mark').length).toBe(0);
    expect(root.textContent).toBe('find me in here');
    expect(root.childNodes.length).toBe(1);
    expect(root.firstChild?.nodeType).toBe(Node.TEXT_NODE);
  });

  it('unwrapMarks is a no-op without highlights', () => {
    const { root } = host('plain text');
    expect(() => unwrapMarks(root)).not.toThrow();
    expect(root.textContent).toBe('plain text');
  });
});
