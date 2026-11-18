import { lineOf, offsetOf, supportsGoto, symbolAt } from './explorer-editor';

describe('explorer-editor helpers', () => {
  it('supportsGoto covers rs/ts/js/php/go/py only', () => {
    expect(supportsGoto('src/main.rs')).toBeTrue();
    expect(supportsGoto('a/b/App.tsx')).toBeTrue();
    expect(supportsGoto('x.PHP')).toBeTrue();
    expect(supportsGoto('cmd/main.go')).toBeTrue();
    expect(supportsGoto('README.md')).toBeFalse();
    expect(supportsGoto('Makefile')).toBeFalse();
    expect(supportsGoto(null)).toBeFalse();
  });

  it('symbolAt finds the identifier around the caret', () => {
    const text = 'let x = fooBar(1);';
    expect(symbolAt(text, 8)).toBe('fooBar'); // start
    expect(symbolAt(text, 11)).toBe('fooBar'); // middle
    expect(symbolAt(text, 14)).toBe('fooBar'); // right after the last char
    expect(symbolAt(text, 7)).toBeNull(); // on the space before
    expect(symbolAt(text, 15)).toBeNull(); // on a digit literal
  });

  it('symbolAt strips PHP sigils and stops at namespace separators', () => {
    expect(symbolAt('$this->total', 2)).toBe('this');
    expect(symbolAt('$this->total', 9)).toBe('total');
    expect(symbolAt('new App\\Service\\UserService()', 20)).toBe('UserService');
  });

  it('offsetOf / lineOf convert between positions', () => {
    const text = 'ab\ncde\nf';
    expect(offsetOf(text, 1, 1)).toBe(0);
    expect(offsetOf(text, 2, 2)).toBe(4);
    expect(offsetOf(text, 2, 99)).toBe(6); // clamped to the line end
    expect(offsetOf(text, 9, 1)).toBe(text.length);
    expect(lineOf(text, 0)).toBe(1);
    expect(lineOf(text, 4)).toBe(2);
    expect(lineOf(text, 7)).toBe(3);
  });
});
