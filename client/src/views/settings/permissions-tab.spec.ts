/**
 * Permissions tab specs - WP-BROWSER2 (F7-6): the "Browser display" control
 * reads `config.browser.display` and saves it to the *global* config layer.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import {
  ConfigResponse,
  SafetyCategory,
  ToolSafetyEntry,
  ToolSafetyResponse,
} from '../../core/engine.dtos';
import { ToolSafetyStore } from '../../core/tool-safety.store';
import {
  DEFAULT_BROWSER_DISPLAY,
  PermissionsTab,
  groupBySafety,
  windowPositionRightOf,
} from './permissions-tab';
import { SettingsStore } from './settings.store';

function safetyEntry(
  name: string,
  category: SafetyCategory,
  extra: Partial<ToolSafetyEntry> = {},
): ToolSafetyEntry {
  return { name, source: 'built-in', category, default_category: category, is_override: false, ...extra };
}

function safetyResponse(
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

function configResponse(browser?: Record<string, unknown>): ConfigResponse {
  return {
    config: {
      model: 'zai/glm-4.6',
      provider: 'zai',
      max_tokens: 4096,
      models: {},
      permission: { rules: [] },
      mcp: {},
      skills: {},
      terminal: {},
      runtimes: {},
      ...(browser ? { browser } : {}),
    },
    providers: [],
    skills: [],
    mcp: [],
    agents: [],
    runtimes: { python: '', python3: '', node: '', php: '', docker: '', git: '' },
    files: {
      global: { exists: false, path: '', content: '{}' },
      project: { exists: false, path: '', content: '{}' },
    },
  };
}

describe('windowPositionRightOf (F7-6)', () => {
  it('places the browser window right of the app on the same top edge', () => {
    expect(windowPositionRightOf({ x: 100, y: 50, width: 1280 })).toEqual([1388, 50]);
    expect(windowPositionRightOf({ x: 0.4, y: 0.6, width: 10 }, 0)).toEqual([10, 1]);
    expect(windowPositionRightOf(null)).toBeNull();
  });
});

describe('PermissionsTab - Browser display (F7-6)', () => {
  let fixture: ComponentFixture<PermissionsTab>;
  let component: PermissionsTab;
  let engine: EngineClient;

  async function setup(browser?: Record<string, unknown>): Promise<void> {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [PermissionsTab],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    spyOn(engine, 'getConfig').and.returnValue(Promise.resolve(configResponse(browser)));
    spyOn(engine, 'putConfig').and.callFake((_dir: string, delta: unknown) =>
      Promise.resolve(configResponse((delta as { browser: Record<string, unknown> }).browser)),
    );
    spyOn(engine, 'getToolSafety').and.returnValue(Promise.resolve(safetyResponse([])));
    fixture = TestBed.createComponent(PermissionsTab);
    component = fixture.componentInstance;
    await component.store.load('C:/tmp/project');
    fixture.detectChanges();
  }

  function select(): HTMLSelectElement {
    return (fixture.nativeElement as HTMLElement).querySelector<HTMLSelectElement>(
      '[data-testid="browser-display-select"]',
    )!;
  }

  afterEach(() => fixture?.destroy());

  it('defaults to headed when the config has no browser section', async () => {
    await setup();
    expect(component.browserDisplay()).toBe('headed');
    expect(DEFAULT_BROWSER_DISPLAY).toBe('headed');
    expect(select()).not.toBeNull();
    expect(Array.from(select().options).map((o) => o.value)).toEqual([
      'headed',
      'viewer',
      'drawer',
    ]);
  });

  it('reflects the configured display mode and ignores garbage', async () => {
    await setup({ display: 'viewer' });
    expect(component.browserDisplay()).toBe('viewer');
    await setup({ display: 'hologram' });
    expect(component.browserDisplay()).toBe('headed');
  });

  it('saves the chosen mode to the global config layer', async () => {
    await setup();
    await component.setBrowserDisplay('drawer');
    expect(engine.putConfig).toHaveBeenCalledWith(
      'C:/tmp/project',
      { browser: { display: 'drawer' } },
      { scope: 'global' },
    );
    expect(component.browserDisplay()).toBe('drawer');
    expect(component.store.saved()).toContain('Browser display saved');
  });

  it('does not store a window position outside the desktop shell', async () => {
    await setup();
    await component.setBrowserDisplay('headed');
    const delta = (engine.putConfig as jasmine.Spy).calls.mostRecent().args[1] as {
      browser: Record<string, unknown>;
    };
    expect(delta.browser['display']).toBe('headed');
    expect(delta.browser['windowPosition']).toBeUndefined();
  });

  it('falls back to headed for an unknown value and reports engine errors', async () => {
    await setup({ display: 'viewer' });
    (engine.putConfig as jasmine.Spy).and.returnValue(Promise.reject(new Error('boom')));
    await component.setBrowserDisplay('nonsense');
    expect((engine.putConfig as jasmine.Spy).calls.mostRecent().args[1]).toEqual({
      browser: { display: 'headed' },
    });
    expect(component.store.error()).toContain('boom');
    expect(component.store.saving()).toBeFalse();
  });

  it('is a no-op while another save is running', async () => {
    await setup();
    component.store.saving.set(true);
    await component.setBrowserDisplay('viewer');
    expect(engine.putConfig).not.toHaveBeenCalled();
  });
});

describe('groupBySafety (F7-7)', () => {
  it('buckets tools by effective category in display order, skipping empty buckets', () => {
    const groups = groupBySafety([
      safetyEntry('rm', 'dangerous'),
      safetyEntry('read_file', 'safe'),
      safetyEntry('mcp__x__y', 'uncategorized', { source: 'mcp:x' }),
      safetyEntry('bash', 'dangerous'),
    ]);
    expect(groups.map((g) => g.category)).toEqual(['safe', 'dangerous', 'uncategorized']);
    expect(groups[1].tools.map((t) => t.name)).toEqual(['rm', 'bash']);
  });
});

describe('PermissionsTab - Tool safety (F7-7)', () => {
  let fixture: ComponentFixture<PermissionsTab>;
  let component: PermissionsTab;
  let engine: EngineClient;
  let safety: ToolSafetyStore;

  const TOOLS: ToolSafetyEntry[] = [
    safetyEntry('read_file', 'safe'),
    safetyEntry('bash', 'caution', { default_category: 'dangerous', is_override: true, override_pattern: 'bash' }),
    safetyEntry('fetch', 'caution'),
    safetyEntry('mcp__jira__search', 'uncategorized', { source: 'mcp:jira' }),
    safetyEntry('plugin_thing', 'uncategorized', { source: 'plugin' }),
  ];

  async function setup(tools: ToolSafetyEntry[] = TOOLS): Promise<void> {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [PermissionsTab],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    safety = TestBed.inject(ToolSafetyStore);
    spyOn(engine, 'getConfig').and.returnValue(Promise.resolve(configResponse()));
    spyOn(engine, 'getToolSafety').and.returnValue(
      Promise.resolve(safetyResponse(tools, { global: { bash: 'caution' }, project: {} })),
    );
    spyOn(engine, 'putToolSafety').and.callFake(
      (_dir: string, overrides: Record<string, SafetyCategory | null>) => {
        const next = tools.map((t) => {
          const patch = overrides[t.name];
          if (patch === undefined) {
            return t;
          }
          return patch === null
            ? { ...t, category: t.default_category, is_override: false, override_pattern: undefined }
            : { ...t, category: patch, is_override: true, override_pattern: t.name };
        });
        return Promise.resolve(safetyResponse(next));
      },
    );
    fixture = TestBed.createComponent(PermissionsTab);
    component = fixture.componentInstance;
    await component.store.load('C:/tmp/project');
    await safety.load('C:/tmp/project');
    fixture.detectChanges();
    await fixture.whenStable();
  }

  function root(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  afterEach(() => fixture?.destroy());

  it('loads the tool list for the settings directory', async () => {
    await setup();
    expect(engine.getToolSafety).toHaveBeenCalledWith('C:/tmp/project');
    expect(root().querySelector('[data-testid="tool-safety-card"]')).not.toBeNull();
  });

  it('lists the uncategorized tools first with a category select per row', async () => {
    await setup();
    const section = root().querySelector('[data-testid="tool-safety-uncategorized"]')!;
    expect(section.textContent).toContain('Uncategorized tools (2)');
    const rows = section.querySelectorAll('tbody tr');
    expect(rows.length).toBe(2);
    expect(rows[0].getAttribute('data-tool')).toBe('mcp__jira__search');
    expect(rows[0].textContent).toContain('mcp:jira');
    expect(rows[1].textContent).toContain('plugin');
    const select = section.querySelector<HTMLSelectElement>(
      '[data-testid="safety-select-mcp__jira__search"]',
    )!;
    expect(Array.from(select.options).map((o) => o.value)).toEqual([
      'safe',
      'caution',
      'dangerous',
      'uncategorized',
    ]);
  });

  it('shows "every tool is categorized" when nothing is gray', async () => {
    await setup([safetyEntry('read_file', 'safe')]);
    const section = root().querySelector('[data-testid="tool-safety-uncategorized"]')!;
    expect(section.textContent).toContain('Every tool is categorized');
    expect(section.querySelector('table')).toBeNull();
  });

  it('groups every tool by category with source and default columns', async () => {
    await setup();
    expect(root().querySelector('[data-testid="tool-safety-group-safe"]')!.textContent).toContain(
      'read_file',
    );
    const caution = root().querySelector('[data-testid="tool-safety-group-caution"]')!;
    expect(caution.querySelectorAll('tbody tr').length).toBe(2);
    const bash = caution.querySelector('[data-tool="bash"]')!;
    expect(bash.classList.contains('overridden')).toBeTrue();
    expect(bash.querySelector('.default')!.textContent).toContain('Dangerous');
    expect(bash.querySelector('.override-badge')!.textContent).toContain('override');
    expect(bash.querySelector('[data-testid="safety-reset-bash"]')).not.toBeNull();
    // A non-overridden row has no reset button.
    const fetch = caution.querySelector('[data-tool="fetch"]')!;
    expect(fetch.querySelector('button')).toBeNull();
    expect(root().querySelector('[data-testid="tool-safety-group-dangerous"]')).toBeNull();
  });

  it('saves a category change through PUT /tools/safety (global) and re-groups', async () => {
    await setup();
    await component.setToolCategory('mcp__jira__search', 'safe');
    expect(engine.putToolSafety).toHaveBeenCalledWith(
      'C:/tmp/project',
      { mcp__jira__search: 'safe' },
      { scope: 'global' },
    );
    fixture.detectChanges();
    await fixture.whenStable();
    expect(component.uncategorizedTools().map((t) => t.name)).toEqual(['plugin_thing']);
    expect(
      root().querySelector('[data-testid="tool-safety-group-safe"] [data-tool="mcp__jira__search"]'),
    ).not.toBeNull();
    expect(component.store.saved()).toContain('Tool safety saved');
  });

  it('ignores a garbage category value', async () => {
    await setup();
    await component.setToolCategory('bash', 'green');
    expect(engine.putToolSafety).not.toHaveBeenCalled();
  });

  it('reset restores the default category', async () => {
    await setup();
    await component.resetToolCategory('bash');
    expect(engine.putToolSafety).toHaveBeenCalledWith('C:/tmp/project', { bash: null }, { scope: 'global' });
    expect(safety.categoryOf('bash')).toBe('dangerous');
  });

  it('surfaces an engine error in the settings banner', async () => {
    await setup();
    (engine.putToolSafety as jasmine.Spy).and.returnValue(Promise.reject(new Error('boom')));
    await component.setToolCategory('fetch', 'safe');
    expect(component.store.error()).toContain('boom');
    expect(component.store.saved()).toBeNull();
  });
});
