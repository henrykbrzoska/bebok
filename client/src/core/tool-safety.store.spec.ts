/**
 * F7-7: `ToolSafetyStore` - the directory-scoped cache of `GET /tools/safety`
 * that colours historical tool calls and backs the Settings "Tool safety"
 * table.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineClient } from './engine-client.service';
import { SafetyCategory, ToolSafetyEntry, ToolSafetyResponse } from './engine.dtos';
import { ToolSafetyStore } from './tool-safety.store';

function entry(
  name: string,
  category: SafetyCategory,
  extra: Partial<ToolSafetyEntry> = {},
): ToolSafetyEntry {
  return {
    name,
    source: 'built-in',
    category,
    default_category: category,
    is_override: false,
    ...extra,
  };
}

function response(
  tools: ToolSafetyEntry[],
  overrides: ToolSafetyResponse['overrides'] = { global: {}, project: {} },
): ToolSafetyResponse {
  return {
    tools,
    uncategorized: tools.filter((t) => t.category === 'uncategorized').length,
    categories: ['safe', 'caution', 'dangerous', 'uncategorized'],
    overrides,
  };
}

describe('ToolSafetyStore (F7-7)', () => {
  let store: ToolSafetyStore;
  let engine: EngineClient;

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    engine = TestBed.inject(EngineClient);
    store = TestBed.inject(ToolSafetyStore);
  });

  it('loads the list for a directory and answers categoryOf by name', async () => {
    spyOn(engine, 'getToolSafety').and.returnValue(
      Promise.resolve(
        response([
          entry('read_file', 'safe'),
          entry('bash', 'dangerous'),
          entry('mcp__srv__x', 'uncategorized', { source: 'mcp:srv' }),
        ]),
      ),
    );
    expect(store.categoryOf('read_file')).toBeNull();
    await store.load('C:/tmp/project');
    expect(engine.getToolSafety).toHaveBeenCalledWith('C:/tmp/project');
    expect(store.directory()).toBe('C:/tmp/project');
    expect(store.categoryOf('read_file')).toBe('safe');
    expect(store.categoryOf('bash')).toBe('dangerous');
    expect(store.categoryOf('mcp__srv__x')).toBe('uncategorized');
    expect(store.categoryOf('nope')).toBeNull();
    expect(store.uncategorizedCount()).toBe(1);
    expect(store.uncategorized().map((e) => e.name)).toEqual(['mcp__srv__x']);
  });

  it('ensure() only fetches for a new directory', async () => {
    const spy = spyOn(engine, 'getToolSafety').and.returnValue(
      Promise.resolve(response([entry('read_file', 'safe')])),
    );
    await store.ensure('C:/a');
    await store.ensure('C:/a');
    expect(spy).toHaveBeenCalledTimes(1);
    await store.ensure('C:/b');
    expect(spy).toHaveBeenCalledTimes(2);
    await store.ensure(null);
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it('resync() re-fetches the cached directory (config.changed)', async () => {
    const spy = spyOn(engine, 'getToolSafety').and.returnValue(
      Promise.resolve(response([entry('read_file', 'safe')])),
    );
    await store.resync();
    expect(spy).not.toHaveBeenCalled();
    await store.load('C:/a');
    await store.resync();
    expect(spy).toHaveBeenCalledTimes(2);
  });

  it('records an engine error without dropping the previous list', async () => {
    spyOn(engine, 'getToolSafety').and.returnValues(
      Promise.resolve(response([entry('read_file', 'safe')])),
      Promise.reject(new Error('engine GET /tools/safety -> 500')),
    );
    await store.load('C:/a');
    await store.load('C:/a');
    expect(store.error()).toContain('500');
    expect(store.categoryOf('read_file')).toBe('safe');
    expect(store.loading()).toBeFalse();
  });

  it('setCategory writes a global override and applies the returned list', async () => {
    spyOn(engine, 'getToolSafety').and.returnValue(
      Promise.resolve(response([entry('bash', 'dangerous')])),
    );
    const put = spyOn(engine, 'putToolSafety').and.returnValue(
      Promise.resolve(
        response(
          [entry('bash', 'caution', { default_category: 'dangerous', is_override: true, override_pattern: 'bash' })],
          { global: { bash: 'caution' }, project: {} },
        ),
      ),
    );
    await store.load('C:/a');
    await store.setCategory('bash', 'caution');
    expect(put).toHaveBeenCalledWith('C:/a', { bash: 'caution' }, { scope: 'global' });
    expect(store.categoryOf('bash')).toBe('caution');
    expect(store.overrideScope('bash')).toBe('global');
    expect(store.overrideScope('read_file')).toBeNull();
  });

  it('resetCategory removes the override from every layer that has one', async () => {
    spyOn(engine, 'getToolSafety').and.returnValue(
      Promise.resolve(
        response(
          [entry('bash', 'safe', { default_category: 'dangerous', is_override: true })],
          { global: { bash: 'caution' }, project: { bash: 'safe' } },
        ),
      ),
    );
    const put = spyOn(engine, 'putToolSafety').and.callFake(
      (_dir: string, overrides: Record<string, SafetyCategory | null>, opts?: { scope?: string }) => {
        const layers = { ...store.layerOverrides() };
        if (opts?.scope === 'project') {
          layers.project = {};
        } else {
          layers.global = {};
        }
        const done = Object.keys(layers.project).length === 0 && Object.keys(layers.global).length === 0;
        return Promise.resolve(
          response(
            [
              done
                ? entry('bash', 'dangerous')
                : entry('bash', 'caution', { default_category: 'dangerous', is_override: true }),
            ],
            layers,
          ),
        );
      },
    );
    await store.load('C:/a');
    expect(store.overrideScope('bash')).toBe('project');
    await store.resetCategory('bash');
    expect(put).toHaveBeenCalledTimes(2);
    expect(put.calls.argsFor(0)).toEqual(['C:/a', { bash: null }, { scope: 'project' }]);
    expect(put.calls.argsFor(1)).toEqual(['C:/a', { bash: null }, { scope: 'global' }]);
    expect(store.categoryOf('bash')).toBe('dangerous');
    expect(store.overrideScope('bash')).toBeNull();
  });

  it('resetCategory of a global-only override writes the global layer once', async () => {
    spyOn(engine, 'getToolSafety').and.returnValue(
      Promise.resolve(
        response([entry('bash', 'caution', { default_category: 'dangerous', is_override: true })], {
          global: { bash: 'caution' },
          project: {},
        }),
      ),
    );
    const put = spyOn(engine, 'putToolSafety').and.returnValue(
      Promise.resolve(response([entry('bash', 'dangerous')])),
    );
    await store.load('C:/a');
    await store.resetCategory('bash');
    expect(put).toHaveBeenCalledTimes(1);
    expect(put).toHaveBeenCalledWith('C:/a', { bash: null }, { scope: 'global' });
  });

  it('does not write without a loaded directory', async () => {
    const put = spyOn(engine, 'putToolSafety');
    await store.setCategory('bash', 'safe');
    expect(put).not.toHaveBeenCalled();
  });
});
