import { fuzzyMatch, isClassKind, matchFile, parseGotoQuery, rankFiles } from './goto-match';
import { navSegments } from './nav-bar';

describe('goto-match', () => {
  it('parses a trailing line / column', () => {
    expect(parseGotoQuery('foo.ts:12:3')).toEqual({ pattern: 'foo.ts', line: 12, column: 3 });
    expect(parseGotoQuery(' a b*c ')).toEqual({
      pattern: 'abc',
      line: undefined,
      column: undefined,
    });
  });

  it('matches camel humps and word starts', () => {
    const m = fuzzyMatch('fif', 'FindInFiles');
    expect(m?.indices).toEqual([0, 4, 6]);
    expect(fuzzyMatch('fif', 'find-in-files.ts')?.indices).toEqual([0, 5, 8]);
    expect(fuzzyMatch('xyz', 'FindInFiles')).toBeNull();
  });

  it('prefers word-start matches over scattered ones', () => {
    const humps = fuzzyMatch('es', 'explorer.spec.ts')!;
    const scattered = fuzzyMatch('es', 'messages.ts')!;
    expect(humps.score).toBeGreaterThan(scattered.score);
  });

  it('splits dir/name patterns', () => {
    expect(matchFile('expl/goto', 'client/src/views/explorer/goto-dialog.ts')).not.toBeNull();
    expect(matchFile('core/goto', 'client/src/views/explorer/goto-dialog.ts')).toBeNull();
  });

  it('ranks an exact name first', () => {
    const files = ['src/app.config.ts', 'src/app.ts', 'src/apple/pie.ts'];
    expect(rankFiles(files, 'app.ts', 10)[0].path).toBe('src/app.ts');
  });

  it('classifies class-like kinds', () => {
    expect(isClassKind('struct_item')).toBeTrue();
    expect(isClassKind('class_declaration')).toBeTrue();
    expect(isClassKind('function_item')).toBeFalse();
  });

  it('builds breadcrumb segments', () => {
    expect(navSegments('proj', 'a/b/c.ts').map((s) => [s.name, s.path, s.isDir])).toEqual([
      ['proj', '', true],
      ['a', 'a', true],
      ['b', 'a/b', true],
      ['c.ts', 'a/b/c.ts', false],
    ]);
  });
});
