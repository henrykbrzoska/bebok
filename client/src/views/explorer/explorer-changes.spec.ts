import { diffToLineChanges, gutterClassFor, hunksToLineChanges, parseDiffHunks } from './explorer-changes';

describe('explorer-changes', () => {
  it('parses @@ headers', () => {
    const hunks = parseDiffHunks('@@ -1,3 +1,4 @@\n@@ -10,0 +11,2 @@\n');
    expect(hunks.length).toBe(2);
    expect(hunks[0]).toEqual({ oldStart: 1, oldLines: 3, newStart: 1, newLines: 4 });
  });

  it('classifies added vs modified vs removed', () => {
    const ch = hunksToLineChanges([
      { oldStart: 5, oldLines: 0, newStart: 5, newLines: 2 },
      { oldStart: 1, oldLines: 2, newStart: 1, newLines: 2 },
      { oldStart: 9, oldLines: 3, newStart: 9, newLines: 0 },
    ]);
    expect([...ch.added]).toEqual([5, 6]);
    expect([...ch.modified]).toEqual([1, 2]);
    expect(ch.removed.has(9)).toBeTrue();
  });

  it('diffToLineChanges + gutterClassFor', () => {
    const ch = diffToLineChanges('@@ -1,0 +1,1 @@\n');
    expect(gutterClassFor(ch, 1)).toBe('gutter-add');
    expect(gutterClassFor(ch, 2)).toBe('');
  });
});
