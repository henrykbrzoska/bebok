/**
 * The Delegation block holds a single numeric field: max_concurrent
 * (1-16, default 3). Saving writes a `PUT /config` delta and clears legacy
 * `mode` / `model_policy` keys from the project layer.
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
      ...(delegation ? { delegation: { max_concurrent: 3, ...delegation } } : {}),
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

describe('readLayerDelegation', () => {
  it('extracts max_concurrent and ignores garbage / legacy keys', () => {
    expect(readLayerDelegation('{}')).toBeNull();
    expect(readLayerDelegation('not json')).toBeNull();
    expect(readLayerDelegation('{"delegation":"off"}')).toBeNull();
    expect(readLayerDelegation('{"delegation":{"mode":"always","model_policy":"cheaper"}}')).toBeNull();
    expect(readLayerDelegation('{"delegation":{"max_concurrent":5}}')).toEqual({
      max_concurrent: 5,
    });
    expect(readLayerDelegation('{"delegation":{"maxConcurrent":5}}')).toEqual({
      max_concurrent: 5,
    });
  });
});

describe('DelegationBlock', () => {
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
    lastDelta = undefined;
    lastOpts = undefined;
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

  it('defaults to 3 when the engine has no delegation section', async () => {
    await setup();
    expect(component.effective()).toBe(3);
    expect(component.maxDraft()).toBe(3);
    expect(el<HTMLInputElement>('delegation-max-concurrent').value).toBe('3');
  });

  it('reflects the resolved max_concurrent', async () => {
    await setup({ max_concurrent: 5 });
    expect(component.maxDraft()).toBe(5);
    expect(el<HTMLInputElement>('delegation-max-concurrent').value).toBe('5');
  });

  it('writes the max_concurrent delta', async () => {
    await setup();
    component.maxDraft.set(7);
    component.commitMax();
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { max_concurrent: 7 } });
    expect(lastOpts).toBeUndefined();
    expect(component.store.saved()).toContain('Delegation settings saved');
  });

  it('clears legacy mode/model_policy keys from the project layer on save', async () => {
    await setup({ max_concurrent: 3 }, { delegation: { mode: 'auto', model_policy: 'cheaper' } });
    component.maxDraft.set(5);
    component.commitMax();
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { max_concurrent: 5 } });
    expect(lastOpts).toEqual({ scope: 'project', replace: true });
  });

  it('clamps max_concurrent to 1..16', async () => {
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
  });
});
