/**
 * Permissions tab specs - WP-BROWSER2 (F7-6): the "Browser display" control
 * reads `config.browser.display` and saves it to the *global* config layer.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { ConfigResponse } from '../../core/engine.dtos';
import { DEFAULT_BROWSER_DISPLAY, PermissionsTab, windowPositionRightOf } from './permissions-tab';
import { SettingsStore } from './settings.store';

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
