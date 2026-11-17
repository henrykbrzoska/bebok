import type { FsCompleteItem } from '../../core/engine.dtos';
import {
  filterCompletions,
  isPhpPath,
  popupPlacement,
  prefixAt,
  utf8Offset,
} from './explorer-complete';

const item = (label: string): FsCompleteItem => ({ label, kind: 'variable', source: 'local' });

describe('explorer-complete helpers', () => {
  it('prefixAt returns the identifier ending at the caret', () => {
    expect(prefixAt('let total = fo', 14, false)).toBe('fo');
    expect(prefixAt('let total = fo', 9, false)).toBe('total');
    expect(prefixAt('let total = fo', 7, false)).toBe('tot');
    expect(prefixAt('a + ', 4, false)).toBe('');
  });

  it('prefixAt keeps a leading $ only for PHP', () => {
    expect(prefixAt('echo $us', 8, true)).toBe('$us');
    expect(prefixAt('echo $us', 8, false)).toBe('us');
  });

  it('utf8Offset converts UTF-16 indexes to UTF-8 bytes', () => {
    expect(utf8Offset('abc', 2)).toBe(2);
    expect(utf8Offset('zażół', 3)).toBe(4); // 'z' + 'a' + 'ż' (two bytes)
    expect(utf8Offset('x', 99)).toBe(1);
  });

  it('isPhpPath is case-insensitive', () => {
    expect(isPhpPath('src/App.PHP')).toBeTrue();
    expect(isPhpPath('main.rs')).toBeFalse();
    expect(isPhpPath(null)).toBeFalse();
  });

  it('filterCompletions keeps server order and ignores $ and case', () => {
    const items = [item('$userName'), item('count'), item('userId')];
    expect(filterCompletions(items, 'us').map((i) => i.label)).toEqual(['$userName', 'userId']);
    expect(filterCompletions(items, '$US').map((i) => i.label)).toEqual(['$userName', 'userId']);
    expect(filterCompletions(items, 'zz')).toEqual([]);
  });

  it('popupPlacement goes below the caret when there is room', () => {
    const pos = popupPlacement({ top: 40, left: 100, lineHeight: 20 }, { width: 800, height: 600 });
    expect(pos).toEqual({ left: 100, top: 60 });
  });

  it('popupPlacement flips above the caret near the bottom edge', () => {
    const pos = popupPlacement({ top: 500, left: 100, lineHeight: 20 }, { width: 800, height: 560 });
    expect(pos.top).toBeUndefined();
    expect(pos.bottom).toBe(60);
  });

  it('popupPlacement clamps the popup inside the editor width', () => {
    const pos = popupPlacement({ top: 0, left: 790, lineHeight: 20 }, { width: 800, height: 600 });
    expect(pos.left).toBe(480);
  });
});
