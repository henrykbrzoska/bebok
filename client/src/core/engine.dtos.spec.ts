/**
 * F7-1: `safetyLevel()`, the pure mapping from a tool part's resolved
 * `permission`/`mutating` fields to the safety-dot colour tier.
 */

import { ToolPart, safetyLevel } from './engine.dtos';

function part(permission?: 'allow' | 'ask' | 'deny', mutating?: boolean): ToolPart {
  return {
    type: 'tool',
    id: 't1',
    name: 'read_file',
    state: { state: 'completed', input: {}, output: 'ok', title: 'read_file' },
    permission,
    mutating,
  };
}

describe('safetyLevel (F7-1)', () => {
  it('is null when the permission/mutating fields are not resolved yet', () => {
    expect(safetyLevel(part(undefined, undefined))).toBeNull();
    expect(safetyLevel(part('allow', undefined))).toBeNull();
    expect(safetyLevel(part(undefined, false))).toBeNull();
  });

  it('is green for an auto-allowed, non-mutating call', () => {
    expect(safetyLevel(part('allow', false))).toBe('green');
  });

  it('is yellow for a call that required "ask" and was approved', () => {
    expect(safetyLevel(part('ask', false))).toBe('yellow');
  });

  it('is orange for a mutating call even when auto-allowed', () => {
    expect(safetyLevel(part('allow', true))).toBe('orange');
  });

  it('is orange for a mutating call that required "ask"', () => {
    expect(safetyLevel(part('ask', true))).toBe('orange');
  });

  it('is orange when the call matched a deny, regardless of mutating', () => {
    expect(safetyLevel(part('deny', false))).toBe('orange');
    expect(safetyLevel(part('deny', true))).toBe('orange');
  });
});
