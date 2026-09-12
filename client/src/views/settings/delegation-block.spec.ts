/**
 * WP-DELEGATION (F8-2): the Delegation block reads `config.delegation`,
 * writes deltas to the chosen scope and detects a project override from the
 * raw project layer.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { ConfigResponse, DelegationConfig } from '../../core/engine.dtos';
import { DelegationBlock, readLayerDelegation } from './delegation-block';
import { SettingsStore } from './settings.store';

function configResponse(
  delegation?: Partial<DelegationConfig>,
  projectLayer: Record<string, unknown> = {},
): ConfigResponse {
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
      ...(delegation ? { delegation: { mode: 'auto', max_concurrent: 3, ...delegation } } : {}),
    },
    providers: [{ name: 'zai', kind: 'openai', models: ['glm-4.6', 'glm-4.5'], has_key: true } as never],
    skills: [],
    mcp: [],
    agents: [],
    runtimes: { python: '', python3: '', node: '', php: '', docker: '', git: '' },
    files: {
      global: { exists: false, path: '', content: '{}' },
      project: { exists: true, path: '', content: JSON.stringify(projectLayer) },
    },
  };
}

describe('readLayerDelegation (WP-DELEGATION)', () => {
  it('extracts the keys a layer sets and ignores garbage', () => {
    expect(readLayerDelegation('{}')).toBeNull();
    expect(readLayerDelegation('not json')).toBeNull();
    expect(readLayerDelegation('{"delegation":"off"}')).toBeNull();
    expect(readLayerDelegation('{"delegation":{"mode":"always"}}')).toEqual({ mode: 'always' });
    expect(
      readLayerDelegation('{"delegation":{"mode":"sometimes","maxConcurrent":5,"model":""}}'),
    ).toEqual({ max_concurrent: 5, model: '' });
  });
});

describe('DelegationBlock (WP-DELEGATION)', () => {
  let fixture: ComponentFixture<DelegationBlock>;
  let component: DelegationBlock;
  let engine: EngineClient;
  let lastDelta: unknown;
  let lastOpts: unknown;

  async function setup(
    delegation?: Partial<DelegationConfig>,
    projectLayer: Record<string, unknown> = {},
  ): Promise<void> {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [DelegationBlock],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    spyOn(engine, 'getConfig').and.returnValue(
      Promise.resolve(configResponse(delegation, projectLayer)),
    );
    spyOn(engine, 'putConfig').and.callFake((_dir: string, delta: unknown, opts?: unknown) => {
      lastDelta = delta;
      lastOpts = opts;
      const d = (delta as { delegation?: Partial<DelegationConfig> }).delegation ?? {};
      return Promise.resolve(configResponse({ ...delegation, ...d }, projectLayer));
    });
    fixture = TestBed.createComponent(DelegationBlock);
    component = fixture.componentInstance;
    await component.store.load('C:/tmp/project');
    fixture.detectChanges();
  }

  function el<T extends HTMLElement>(testId: string): T {
    return (fixture.nativeElement as HTMLElement).querySelector<T>(`[data-testid="${testId}"]`)!;
  }

  afterEach(() => fixture?.destroy());

  it('defaults to auto / 3 / no model when the engine has no delegation section', async () => {
    await setup();
    expect(component.effective()).toEqual({ mode: 'auto', max_concurrent: 3, model: null });
    expect(el<HTMLInputElement>('delegation-mode-auto').checked).toBeTrue();
    expect(el<HTMLInputElement>('delegation-mode-off').checked).toBeFalse();
    expect(component.projectOverride()).toBeNull();
    expect(fixture.nativeElement.textContent).toContain('Using the global setting');
  });

  it('reflects the resolved config and the project override', async () => {
    await setup({ mode: 'always', max_concurrent: 5, model: 'zai/glm-4.5' }, {
      delegation: { mode: 'always' },
    });
    expect(el<HTMLInputElement>('delegation-mode-always').checked).toBeTrue();
    expect(component.maxDraft()).toBe(5);
    expect(el<HTMLSelectElement>('delegation-model').value).toBe('zai/glm-4.5');
    expect(component.projectOverride()).toEqual({ mode: 'always' });
    expect(el('delegation-override-note').textContent).toContain('mode');
  });

  it('writes the mode to the project layer by default and to global when chosen', async () => {
    await setup();
    component.setMode('always');
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { mode: 'always' } });
    expect(lastOpts).toBeUndefined();
    expect(component.store.saved()).toContain('Delegation settings saved');

    component.scope.set('global');
    component.setMode('off');
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { mode: 'off' } });
    expect(lastOpts).toEqual({ scope: 'global' });
  });

  it('clamps and saves max_concurrent, and clears the model with an empty string', async () => {
    await setup();
    component.maxDraft.set(99);
    component.commitMax();
    await fixture.whenStable();
    expect(component.maxDraft()).toBe(16);
    expect(lastDelta).toEqual({ delegation: { max_concurrent: 16 } });

    component.maxDraft.set(0);
    component.commitMax();
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { max_concurrent: 1 } });

    component.setModel('zai/glm-4.5');
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { model: 'zai/glm-4.5' } });
    component.setModel('');
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { model: '' } });
  });

  it('removes the project override by rewriting the project layer without it', async () => {
    await setup({ mode: 'off' }, { model: 'x', delegation: { mode: 'off' } });
    await component.removeProjectOverride();
    expect(lastDelta).toEqual({ model: 'x' });
    expect(lastOpts).toEqual({ scope: 'project', replace: true });
  });
});
