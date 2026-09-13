/**
 * WP-DELEGATION (F8-2): the Delegation block reads `config.delegation`,
 * writes deltas to the chosen scope and detects a project override from the
 * raw project layer.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { ConfigResponse, DelegationConfig, DelegationModelsResponse } from '../../core/engine.dtos';
import { DelegationBlock, policyKindOf, readLayerDelegation } from './delegation-block';
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
    expect(readLayerDelegation('{"delegation":{"model_policy":"cheaper"}}')).toEqual({
      model_policy: 'cheaper',
    });
  });
});

describe('policyKindOf (F9-10)', () => {
  it('maps the keywords, an explicit id and the legacy model key', () => {
    expect(policyKindOf('inherit', null)).toEqual({ kind: 'inherit', model: null });
    expect(policyKindOf('cheaper', 'zai/glm-4.5')).toEqual({ kind: 'cheaper', model: null });
    expect(policyKindOf('openai/gpt-5.6-mini', null)).toEqual({
      kind: 'explicit',
      model: 'openai/gpt-5.6-mini',
    });
    // Legacy `delegation.model` == explicit policy; nothing set == cheaper (engine default).
    expect(policyKindOf(undefined, 'zai/glm-4.5')).toEqual({ kind: 'explicit', model: 'zai/glm-4.5' });
    expect(policyKindOf('', '')).toEqual({ kind: 'cheaper', model: null });
  });
});

describe('DelegationBlock (WP-DELEGATION)', () => {
  let fixture: ComponentFixture<DelegationBlock>;
  let component: DelegationBlock;
  let engine: EngineClient;
  let lastDelta: unknown;
  let lastOpts: unknown;
  let delegationModels: jasmine.Spy;

  const RESOLVED: DelegationModelsResponse = {
    policy: 'cheaper',
    parent_model: 'zai/glm-4.6',
    resolved: 'zai/glm-4.5',
    mappings: [
      { provider: 'zai', model: 'glm-4.6', cheaper: 'zai/glm-4.5' },
      { provider: 'openai', model: 'gpt-5.6-luna', cheaper: null },
    ],
  };

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
    delegationModels = spyOn(engine, 'delegationModels').and.resolveTo(RESOLVED);
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

  it('defaults to auto / 3 / cheaper when the engine has no delegation section', async () => {
    await setup();
    expect(component.effective()).toEqual({ mode: 'auto', max_concurrent: 3, model: null });
    expect(component.policyKind()).toBe('cheaper');
    expect(el<HTMLInputElement>('delegation-policy-cheaper').checked).toBeTrue();
    expect(el<HTMLSelectElement>('delegation-model').disabled).toBeTrue();
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
    // Legacy `model` key == explicit policy.
    expect(component.policyKind()).toBe('explicit');
    expect(el<HTMLInputElement>('delegation-policy-explicit').checked).toBeTrue();
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

  it('F9-10: writes the policy keyword (clearing the legacy model) and reloads the resolved table', async () => {
    await setup({ model: 'zai/glm-4.5' });
    expect(delegationModels).toHaveBeenCalledWith('C:/tmp/project');
    const calls = delegationModels.calls.count();
    component.setPolicy('inherit');
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { model_policy: 'inherit', model: '' } });
    expect(delegationModels.calls.count()).toBe(calls + 1);

    component.setPolicy('cheaper');
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { model_policy: 'cheaper', model: '' } });
    // Same policy again is a no-op.
    const before = lastDelta;
    component.setPolicy('cheaper');
    await fixture.whenStable();
    expect(lastDelta).toBe(before);
  });

  it('F9-10: explicit policy stores the picked model id as the policy string', async () => {
    await setup({ model_policy: 'cheaper' });
    expect(component.policyKind()).toBe('cheaper');
    // Choosing "explicit" with no model yet only enables the select.
    component.setPolicy('explicit');
    await fixture.whenStable();
    await Promise.resolve();
    fixture.detectChanges();
    expect(component.policyKind()).toBe('explicit');
    expect(el<HTMLSelectElement>('delegation-model').disabled).toBeFalse();
    expect(lastDelta).toBeUndefined();

    component.setModel('zai/glm-4.5');
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { model_policy: 'zai/glm-4.5', model: '' } });
    expect(component.policyKind()).toBe('explicit');
    expect(component.explicitDraft()).toBe('zai/glm-4.5');

    // Clearing the explicit model falls back to inherit.
    component.setModel('');
    await fixture.whenStable();
    expect(lastDelta).toEqual({ delegation: { model_policy: 'inherit', model: '' } });
  });

  it('F9-10: renders the resolved cheaper-model table', async () => {
    await setup();
    await fixture.whenStable();
    fixture.detectChanges();
    const rows = Array.from(
      (fixture.nativeElement as HTMLElement).querySelectorAll('[data-testid="delegation-mapping"]'),
    );
    expect(rows.length).toBe(2);
    expect(rows[0].textContent).toContain('zai');
    expect(rows[0].textContent).toContain('glm-4.6');
    expect(rows[0].textContent).toContain('zai/glm-4.5');
    expect(rows[1].textContent).toContain('no cheaper sibling');
    expect(el('delegation-resolved-now').textContent).toContain('zai/glm-4.5');
  });

  it('clamps and saves max_concurrent', async () => {
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

  it('removes the project override by rewriting the project layer without it', async () => {
    await setup({ mode: 'off' }, { model: 'x', delegation: { mode: 'off' } });
    await component.removeProjectOverride();
    expect(lastDelta).toEqual({ model: 'x' });
    expect(lastOpts).toEqual({ scope: 'project', replace: true });
  });
});
