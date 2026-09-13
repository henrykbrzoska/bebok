/**
 * WP-M5 / F10-19: Settings-lite - list → section navigation (back returns
 * to the list, not out of Settings), one render smoke per section, and a
 * provider key saved through the global config layer.
 */

import { Component, provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, RouterOutlet, provideRouter } from '@angular/router';

import { CustomCssService } from '../../../core/custom-css.service';
import { EngineClient } from '../../../core/engine-client.service';
import { ConfigResponse } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { ProviderCatalog } from '../../settings/provider-catalog';
import { SettingsLiteView, isLiteSection } from './settings-lite';

@Component({
  selector: 'test-host',
  imports: [RouterOutlet],
  template: '<router-outlet />',
})
class Host {}

function config(): ConfigResponse {
  return {
    config: {
      model: 'openai/gpt-4.1',
      models: { code: 'openai/gpt-4.1' },
      thinking: 'off',
      permission: { rules: [] },
      ui: { customCss: '' },
    },
    providers: [
      { name: 'openai', kind: 'openai', endpoint: null, api_key: null, models: ['gpt-4.1'], has_key: false },
      { name: 'anthropic', kind: 'anthropic', endpoint: null, api_key: null, models: [], has_key: true },
    ],
    skills: [],
    mcp: [],
    agents: [
      { name: 'code', builtin: true, description: 'Writes code', model: 'openai/gpt-4.1' },
      { name: 'ask', builtin: true },
    ],
    runtimes: { python: 'python', python3: 'python3', node: 'node', php: 'php', docker: 'docker', git: 'git' },
    files: {
      global: { exists: false, path: '/g/config.json', content: '{}' },
      project: { exists: false, path: '/p/.bebok/config.json', content: '{}' },
    },
  } as unknown as ConfigResponse;
}

describe('SettingsLiteView (F10-19)', () => {
  let fixture: ComponentFixture<Host>;
  let router: Router;
  let engine: Record<string, jasmine.Spy | unknown>;

  function el<T extends HTMLElement>(testId: string): T | null {
    return (fixture.nativeElement as HTMLElement).querySelector<T>(`[data-testid="${testId}"]`);
  }

  async function settle(): Promise<void> {
    for (let i = 0; i < 8; i++) {
      await Promise.resolve();
      await fixture.whenStable();
    }
    fixture.detectChanges();
  }

  beforeEach(async () => {
    localStorage.clear();
    engine = {
      connected: signal(true),
      isTauri: signal(false),
      isCapacitor: false,
      connect: jasmine.createSpy('connect').and.resolveTo({}),
      readLastDirectory: () => '/p',
      getConfig: jasmine.createSpy('getConfig').and.resolveTo(config()),
      putConfig: jasmine.createSpy('putConfig').and.resolveTo(config()),
      listModels: jasmine.createSpy('listModels').and.resolveTo({ provider: 'openai', models: ['gpt-4.1', 'o4'] }),
    };
    TestBed.configureTestingModule({
      imports: [Host],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([
          { path: 'm/more', component: Host },
          { path: 'm/more/settings', component: SettingsLiteView },
          { path: 'm/more/settings/:section', component: SettingsLiteView },
        ]),
        { provide: EngineClient, useValue: engine },
        { provide: EventsStore, useValue: { start: () => undefined, state: signal('live') } },
        {
          provide: ProviderCatalog,
          useValue: {
            load: async () => undefined,
            find: () => null,
            isBuiltin: () => true,
            needsApiKey: (id: string) => id !== 'ollama',
            allowedKinds: () => ['openai', 'anthropic'],
            extraFields: () => [],
          },
        },
        { provide: CustomCssService, useValue: { sync: async () => undefined } },
      ],
    });
    router = TestBed.inject(Router);
    fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
    await router.navigateByUrl('/m/more/settings');
    await settle();
  });

  afterEach(() => {
    fixture.destroy();
    localStorage.clear();
  });

  it('renders the four sections and navigates list → section → back to the list', async () => {
    const list = el('settings-lite-list')!;
    expect(list).not.toBeNull();
    const entries = [...list.querySelectorAll('a')].map((a) => a.getAttribute('data-testid'));
    expect(entries).toEqual([
      'settings-lite-providers',
      'settings-lite-agents',
      'settings-lite-appearance',
      'settings-lite-device',
    ]);
    expect(el('settings-lite-directory')!.textContent).toContain('/p');

    el<HTMLAnchorElement>('settings-lite-providers')!.click();
    await settle();
    expect(router.url).toBe('/m/more/settings/providers');
    expect(el('providers-lite-list')).not.toBeNull();
    expect(el('settings-lite-list')).toBeNull();

    el<HTMLButtonElement>('settings-lite-back')!.click();
    await settle();
    expect(router.url).toBe('/m/more/settings');
    expect(el('settings-lite-list')).not.toBeNull();
  });

  it('providers: opens a provider and saves its key to the global layer', async () => {
    await router.navigateByUrl('/m/more/settings/providers');
    await settle();
    const rows = [...el('providers-lite-list')!.querySelectorAll('button')];
    expect(rows.map((r) => r.textContent?.trim().split(/\s+/)[0])).toEqual(['openai', 'anthropic']);
    expect(rows[1].textContent).toContain('configured');
    expect(rows[0].textContent).toContain('no key');

    el<HTMLButtonElement>('provider-openai')!.click();
    await settle();
    expect(el('providers-lite-detail')).not.toBeNull();
    const key = el<HTMLInputElement>('providers-lite-key')!;
    expect(key.type).toBe('password');
    expect(key.value).toBe('');
    key.value = 'sk-test-123';
    key.dispatchEvent(new Event('input'));
    await settle();
    el<HTMLButtonElement>('providers-lite-save')!.click();
    await settle();

    expect(engine['putConfig']).toHaveBeenCalledTimes(1);
    const [dir, delta, opts] = (engine['putConfig'] as jasmine.Spy).calls.mostRecent().args as [
      string,
      { providers: Array<{ name: string; api_key: string | null }> },
      { scope: string },
    ];
    expect(dir).toBe('/p');
    expect(opts).toEqual({ scope: 'global' });
    expect(delta.providers.find((p) => p.name === 'openai')?.api_key).toBe('sk-test-123');
    expect(delta.providers.find((p) => p.name === 'anthropic')?.api_key).toBeNull();
    expect(el('settings-lite-saved')).not.toBeNull();
    // The key never lands in the DOM after the save either.
    expect((fixture.nativeElement as HTMLElement).innerHTML).not.toContain('sk-test-123');

    // Back inside the section returns to the provider list first.
    el<HTMLButtonElement>('providers-lite-list-link')!.click();
    await settle();
    expect(el('providers-lite-list')).not.toBeNull();
    expect(router.url).toBe('/m/more/settings/providers');
  });

  it('agents: renders the per-type models and the agent list', async () => {
    await router.navigateByUrl('/m/more/settings/agents');
    await settle();
    const code = el<HTMLSelectElement>('agents-lite-model-code')!;
    expect(code.value).toBe('openai/gpt-4.1');
    expect(el('agents-lite-list')!.textContent).toContain('Writes code');
    el<HTMLButtonElement>('agents-lite-save')!.click();
    await settle();
    expect(engine['putConfig']).toHaveBeenCalledWith('/p', { models: { code: 'openai/gpt-4.1' } }, { scope: 'global' });
  });

  it('appearance: renders language, tool-call toggle and custom CSS', async () => {
    await router.navigateByUrl('/m/more/settings/appearance');
    await settle();
    expect(el<HTMLSelectElement>('appearance-lite-lang')!.options.length).toBeGreaterThan(5);
    expect(el('appearance-lite-expand')).not.toBeNull();
    expect(el('appearance-lite-css')).not.toBeNull();
  });

  it('device: renders the engine card; the developer card is Capacitor-only', async () => {
    await router.navigateByUrl('/m/more/settings/device');
    await settle();
    expect(el('device-lite-engine')).not.toBeNull();
    expect(el('device-lite-status')!.textContent).toContain('live');
    expect(el('device-lite-developer')).toBeNull();
  });

  it('rejects unknown sections', () => {
    expect(isLiteSection('providers')).toBeTrue();
    expect(isLiteSection('mcp')).toBeFalse();
    expect(isLiteSection(null)).toBeFalse();
  });
});
