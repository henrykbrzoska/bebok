/**
 * Code intelligence card specs: the toggles persist a
 * `{ <section>: { enabled } }` config delta via `PUT /config` and mirror the
 * resolved config back into the row state; the regenerate buttons call the
 * dedicated endpoints and surface cache counts when they succeed.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { ConfigResponse } from '../../core/engine.dtos';
import { CodeFeaturesCard } from './code-features-card';
import { SettingsStore } from './settings.store';

function configResponse(sections?: Record<string, unknown>): ConfigResponse {
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
      ...(sections ? { ...sections } : {}),
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

describe('CodeFeaturesCard', () => {
  let fixture: ComponentFixture<CodeFeaturesCard>;
  let component: CodeFeaturesCard;
  let engine: EngineClient;

  async function setup(sections?: Record<string, unknown>): Promise<void> {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [CodeFeaturesCard],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    spyOn(engine, 'getConfig').and.returnValue(Promise.resolve(configResponse(sections)));
    fixture = TestBed.createComponent(CodeFeaturesCard);
    component = fixture.componentInstance;
    await component.store.load('/tmp/project');
    fixture.detectChanges();
  }

  it('mirrors the resolved config into the toggle signals', async () => {
    await setup({
      code_map: { enabled: true },
      code_graph: { enabled: false },
      ast_search: { enabled: true },
    });
    expect(component.codeMapEnabled()).toBeTrue();
    expect(component.codeGraphEnabled()).toBeFalse();
    expect(component.astSearchEnabled()).toBeTrue();
  });

  it('defaults everything to off on a config without the sections', async () => {
    await setup();
    expect(component.codeMapEnabled()).toBeFalse();
    expect(component.codeGraphEnabled()).toBeFalse();
    expect(component.astSearchEnabled()).toBeFalse();
  });

  it('toggling the code map persists { code_map: { enabled } } and adopts the response', async () => {
    await setup();
    spyOn(engine, 'putConfig').and.callFake((_dir: string, delta: unknown) =>
      Promise.resolve(configResponse(delta as Record<string, unknown>)),
    );
    spyOn(engine, 'getCodeMap').and.returnValue(
      Promise.reject(new Error('no cache yet')),
    );

    await component.toggleCodeMap(true);

    expect(engine.putConfig).toHaveBeenCalledWith('/tmp/project', { code_map: { enabled: true } });
    expect(component.codeMapEnabled()).toBeTrue();
    expect(component.saving()).toBeFalse();
    expect(component.codeMapEntries()).toBeNull();
  });

  it('toggling the code graph persists { code_graph: { enabled } }', async () => {
    await setup();
    spyOn(engine, 'putConfig').and.callFake((_dir: string, delta: unknown) =>
      Promise.resolve(configResponse(delta as Record<string, unknown>)),
    );
    spyOn(engine, 'getCodeGraph').and.returnValue(
      Promise.resolve({ enabled: true, graph: { modules: [{}, {}], edges: [{}] } }),
    );

    await component.toggleCodeGraph(true);

    expect(engine.putConfig).toHaveBeenCalledWith('/tmp/project', {
      code_graph: { enabled: true },
    });
    expect(component.codeGraphEnabled()).toBeTrue();
    expect(component.codeGraphModules()).toBe(2);
    expect(component.codeGraphEdges()).toBe(1);
  });

  it('toggling AST search persists { ast_search: { enabled } }', async () => {
    await setup();
    spyOn(engine, 'putConfig').and.callFake((_dir: string, delta: unknown) =>
      Promise.resolve(configResponse(delta as Record<string, unknown>)),
    );

    await component.toggleAstSearch(true);

    expect(engine.putConfig).toHaveBeenCalledWith('/tmp/project', {
      ast_search: { enabled: true },
    });
    expect(component.astSearchEnabled()).toBeTrue();
  });

  it('a failed save reports the error and keeps the switch unchanged', async () => {
    await setup();
    spyOn(engine, 'putConfig').and.returnValue(Promise.reject(new Error('engine down')));

    await component.toggleCodeMap(true);

    expect(component.codeMapEnabled()).toBeFalse();
    expect(component.saving()).toBeFalse();
    expect(component.store.error()).toContain('engine down');
  });

  it('regenerate refreshes the code map entry count and shows a toast', async () => {
    await setup({ code_map: { enabled: true } });
    spyOn(engine, 'regenerateCodeMap').and.returnValue(
      Promise.resolve({
        enabled: true,
        map: { entries: [{}, {}, {}] },
        section: 'Project code map:',
      }),
    );

    await component.regenerateCodeMap();

    expect(component.codeMapEntries()).toBe(3);
    expect(component.codeMapRegenerating()).toBeFalse();
  });

  it('regenerate reports a failure without crashing', async () => {
    await setup({ code_graph: { enabled: true } });
    spyOn(engine, 'regenerateCodeGraph').and.returnValue(
      Promise.reject(new Error('scan failed')),
    );

    await component.regenerateCodeGraph();

    expect(component.codeGraphRegenerating()).toBeFalse();
    expect(component.store.error()).toContain('scan failed');
  });
});
