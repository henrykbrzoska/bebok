/**
 * F7-7: `safetyCategory()`, the pure mapping from a tool part to the
 * safety-dot category: the engine-stamped `safety` field first, else the
 * client-side resolver (tool name -> current category from
 * `GET /tools/safety`), else `uncategorized`. The F7-1 `permission` /
 * `mutating` runtime-verdict fields are ignored on purpose.
 */

import { SafetyCategory, ToolPart, isSafetyCategory, safetyCategory } from './engine.dtos';

function part(extra: Partial<ToolPart> = {}): ToolPart {
  return {
    type: 'tool',
    id: 't1',
    name: 'read_file',
    state: { state: 'completed', input: {}, output: 'ok', title: 'read_file' },
    ...extra,
  };
}

describe('safetyCategory (F7-7)', () => {
  it('uses the engine-stamped category when present', () => {
    expect(safetyCategory(part({ safety: 'safe' }))).toBe('safe');
    expect(safetyCategory(part({ safety: 'caution' }))).toBe('caution');
    expect(safetyCategory(part({ safety: 'dangerous' }))).toBe('dangerous');
    expect(safetyCategory(part({ safety: 'uncategorized' }))).toBe('uncategorized');
  });

  it('prefers the stamped category over the resolver', () => {
    const resolve = jasmine.createSpy('resolve').and.returnValue('dangerous' as SafetyCategory);
    expect(safetyCategory(part({ safety: 'safe' }), resolve)).toBe('safe');
    expect(resolve).not.toHaveBeenCalled();
  });

  it('derives the category of a historical part from the tool name', () => {
    const resolve = (name: string): SafetyCategory | null => (name === 'read_file' ? 'safe' : null);
    expect(safetyCategory(part(), resolve)).toBe('safe');
    expect(safetyCategory(part({ name: 'mcp__gone__x' }), resolve)).toBe('uncategorized');
  });

  it('is uncategorized without a stamped category and without a resolver', () => {
    expect(safetyCategory(part())).toBe('uncategorized');
  });

  it('ignores the legacy permission/mutating verdict fields', () => {
    expect(safetyCategory(part({ permission: 'allow', mutating: false }))).toBe('uncategorized');
    expect(safetyCategory(part({ permission: 'ask', mutating: true }))).toBe('uncategorized');
    expect(safetyCategory(part({ permission: 'deny', mutating: true, safety: 'safe' }))).toBe(
      'safe',
    );
  });

  it('treats an unknown stamped value as unstamped', () => {
    const bogus = part({ safety: 'green' as unknown as SafetyCategory });
    expect(safetyCategory(bogus, () => 'caution')).toBe('caution');
    expect(isSafetyCategory('green')).toBeFalse();
    expect(isSafetyCategory('safe')).toBeTrue();
  });
});
