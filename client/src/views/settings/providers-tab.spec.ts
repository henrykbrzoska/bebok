/**
 * Providers tab specs.
 *
 * F3-4: "Test connection" must only exercise the connection - it must not
 *       persist an unconfirmed draft of the form.
 * F3-5: the `kind` select must be disabled for built-in providers and only
 *       editable for genuinely custom ones.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { ConfigResponse } from '../../core/engine.dtos';
import { ProviderCatalog } from './provider-catalog';
import { ProvidersTab } from './providers-tab';
import { SettingsStore } from './settings.store';

function configResponse(): ConfigResponse {
  return {
    config: {
      model: 'zai/glm-4.6',
      provider: 'zai',
      max_tokens: 4096,
      models: {},
      permission: {},
      mcp: {},
      skills: {},
      terminal: {},
      runtimes: {},
    },
    providers: [
      { name: 'openai', kind: 'openai', endpoint: null, api_key: null, models: [], has_key: true },
      { name: 'mycorp', kind: 'openai', endpoint: null, api_key: null, models: [], has_key: false },
    ],
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

describe('ProvidersTab', () => {
  let fixture: ComponentFixture<ProvidersTab>;
  let component: ProvidersTab;
  let engine: EngineClient;

  beforeEach(async () => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [ProvidersTab],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });

    engine = TestBed.inject(EngineClient);
    spyOn(engine, 'getConfig').and.returnValue(Promise.resolve(configResponse()));
    spyOn(engine, 'putConfig').and.returnValue(Promise.resolve(configResponse()));
    spyOn(engine, 'listModels').and.returnValue(
      Promise.resolve({ provider: 'openai', models: ['gpt-4o', 'gpt-4o-mini'] }),
    );

    // The catalog is fetched over HTTP; the static built-in list is enough here.
    const catalog = TestBed.inject(ProviderCatalog);
    spyOn(catalog, 'load').and.returnValue(Promise.resolve());

    fixture = TestBed.createComponent(ProvidersTab);
    component = fixture.componentInstance;
    await component.store.load('C:/tmp/project');
    fixture.detectChanges();
  });

  it('F3-4: "Test connection" does not save the config', async () => {
    // A pending edit that must NOT reach the engine while only testing.
    component.store.updateProvider('openai', { endpoint: 'https://draft.example/v1' });
    component.store.setKeyDraft('openai', 'sk-typed-but-not-saved');

    await component.checkModels(component.store.providers()[0]);

    expect(engine.listModels).toHaveBeenCalled();
    expect(engine.putConfig).not.toHaveBeenCalled();
    expect(component.store.checkedModels()['openai']).toEqual(['gpt-4o', 'gpt-4o-mini']);
    // The user is told that the test ran against the saved configuration.
    expect(component.dirty()).toBeTrue();
  });

  it('F3-4: saving providers is what writes the config', async () => {
    await component.store.saveProviders();
    expect(engine.putConfig).toHaveBeenCalled();
  });

  it('F3-5: the kind select is disabled for a built-in provider', async () => {
    component.select('openai');
    await fixture.whenStable();
    fixture.detectChanges();

    const select = (fixture.nativeElement as HTMLElement).querySelector(
      'select.kind-select',
    ) as HTMLSelectElement;
    expect(select).not.toBeNull();
    expect(select.disabled).withContext('built-in providers keep their kind').toBeTrue();
  });

  it('F3-5: the kind select is editable for a custom provider', async () => {
    component.select('mycorp');
    await fixture.whenStable();
    fixture.detectChanges();

    const select = (fixture.nativeElement as HTMLElement).querySelector(
      'select.kind-select',
    ) as HTMLSelectElement;
    expect(select).not.toBeNull();
    expect(select.disabled).toBeFalse();
    expect(select.options.length).withContext('both protocols offered').toBe(2);
  });

  it('the stored API key is never bound into the DOM', async () => {
    component.store.updateProvider('openai', { api_key: 'sk-secret-from-engine' });
    component.select('openai');
    await fixture.whenStable();
    fixture.detectChanges();

    const input = (fixture.nativeElement as HTMLElement).querySelector(
      'input[type="password"]',
    ) as HTMLInputElement;
    expect(input).not.toBeNull();
    expect(input.value).toBe('');
  });
});
