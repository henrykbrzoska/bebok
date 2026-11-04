/**
 * Providers tab specs.
 *
 * F3-4: "Test connection" must only exercise the connection - it must not
 *       persist an unconfirmed draft of the form.
 * F3-5: the `kind` select must be disabled for built-in providers and only
 *       editable for genuinely custom ones.
 * CLI:  a `kind: "cli"` provider is keyless and endpoint-less, so neither the
 *       API-key nor the base-URL input may render; the "CLI agents" card lists
 *       one row per backend the engine reports.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { CliBackend, ConfigResponse } from '../../core/engine.dtos';
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

function cliBackend(overrides: Partial<CliBackend>): CliBackend {
  return {
    id: 'claude',
    label: 'Claude Code',
    available: true,
    binary: '/usr/local/bin/claude',
    version: '2.0.1',
    capabilities: {
      streaming: true,
      resume: true,
      system_prompt: true,
      tool_events: true,
      usage: true,
      images: false,
      mcp: true,
    },
    defaultModels: ['opus', 'sonnet', 'haiku'],
    envOverride: 'BEBOK_CLAUDE_BIN',
    ...overrides,
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
    spyOn(engine, 'cliBackends').and.returnValue(
      Promise.resolve([
        cliBackend({}),
        cliBackend({ id: 'other', label: 'Other CLI', available: false, binary: null, version: null }),
      ]),
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

  it('renders no API-key or endpoint input for a kind: "cli" provider', async () => {
    component.select('claude-cli');
    await fixture.whenStable();
    fixture.detectChanges();

    const host = fixture.nativeElement as HTMLElement;
    expect(host.querySelector('input[data-testid="provider-api-key"]')).toBeNull();
    expect(host.querySelector('input[data-testid="provider-endpoint"]')).toBeNull();
    // The CLI-specific extra fields take their place.
    expect(host.querySelector('input[data-testid="cli-extra-command"]')).not.toBeNull();
    expect(host.querySelector('input[data-testid="cli-extra-allow-write"]')).not.toBeNull();
    // Writing through the CLI is opt-in, so the warning is always visible.
    expect(host.textContent).toContain(component.t('settings.cliAllowWriteWarning'));
  });

  it('renders one row per CLI backend', async () => {
    await fixture.whenStable();
    fixture.detectChanges();

    const host = fixture.nativeElement as HTMLElement;
    expect(host.querySelectorAll('.cli-row').length).toBe(2);
    expect(host.querySelector('[data-testid="cli-backend-test-claude"]')).not.toBeNull();
    expect(host.querySelector('[data-testid="cli-backend-add-claude"]')).not.toBeNull();
    // An unavailable backend says so instead of printing a missing binary.
    expect(host.textContent).toContain(component.t('settings.cliNotInstalled'));
  });

  it('"Add as provider" writes a keyless cli entry through the config PUT', async () => {
    await fixture.whenStable();
    // Start from a config without the CLI provider so the entry is created here.
    component.store.removeProvider('claude-cli');
    await component.addCliProvider(component.backends()[0]);

    const added = component.store.providers().find((p) => p.name === 'claude-cli');
    expect(added).withContext('the backend becomes a provider').toBeDefined();
    expect(added!.kind).toBe('cli');
    expect(added!.models).toEqual(['opus', 'sonnet', 'haiku']);
    expect(added!.extra).toEqual({
      backend: 'claude',
      mode: 'agent',
      allow_write: false,
      timeout_secs: 900,
    });
    expect(engine.putConfig).toHaveBeenCalled();

    // The new provider is keyless by construction, so no key field appears.
    fixture.detectChanges();
    const host = fixture.nativeElement as HTMLElement;
    expect(host.querySelector('input[data-testid="provider-api-key"]')).toBeNull();
  });
});
