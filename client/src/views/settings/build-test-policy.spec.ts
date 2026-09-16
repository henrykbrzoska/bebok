/**
 * Build & test policy card specs: the radio group stages a pending selection,
 * and an explicit Save button persists `{ verify: { buildTest } }` to the
 * chosen config layer (project by default). The effective mode stays on the
 * value actually in effect when another layer keeps overriding the save.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { ConfigResponse } from '../../core/engine.dtos';
import { BuildTestPolicyCard, DEFAULT_BUILD_TEST_MODE, layerBuildTest } from './build-test-policy';
import { SettingsStore } from './settings.store';

function configResponse(verify?: Record<string, unknown>, projectContent = '{}'): ConfigResponse {
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
      ...(verify ? { verify } : {}),
    },
    providers: [],
    skills: [],
    mcp: [],
    agents: [],
    runtimes: { python: '', python3: '', node: '', php: '', docker: '', git: '' },
    files: {
      global: { exists: false, path: '', content: '{}' },
      project: { exists: projectContent !== '{}', path: '', content: projectContent },
    },
  };
}

describe('layerBuildTest', () => {
  it('reads verify.buildTest out of a raw layer and ignores junk', () => {
    expect(layerBuildTest('{ "verify": { "buildTest": "ask" } }')).toBe('ask');
    expect(layerBuildTest('{ "verify": { "buildTest": "loud" } }')).toBeNull();
    expect(layerBuildTest('{}')).toBeNull();
    expect(layerBuildTest('not json')).toBeNull();
    expect(layerBuildTest(undefined)).toBeNull();
  });
});

describe('BuildTestPolicyCard', () => {
  let fixture: ComponentFixture<BuildTestPolicyCard>;
  let component: BuildTestPolicyCard;
  let engine: EngineClient;

  async function setup(verify?: Record<string, unknown>, projectContent = '{}'): Promise<void> {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [BuildTestPolicyCard],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    spyOn(engine, 'getConfig').and.returnValue(
      Promise.resolve(configResponse(verify, projectContent)),
    );
    spyOn(engine, 'putConfig').and.callFake((_dir: string, delta: unknown) =>
      Promise.resolve(configResponse((delta as { verify: Record<string, unknown> }).verify)),
    );
    fixture = TestBed.createComponent(BuildTestPolicyCard);
    component = fixture.componentInstance;
    await component.store.load('C:/tmp/project');
    fixture.detectChanges();
  }

  function radio(mode: string): HTMLInputElement {
    return (fixture.nativeElement as HTMLElement).querySelector<HTMLInputElement>(
      `[data-testid="build-test-${mode}"]`,
    )!;
  }

  afterEach(() => fixture?.destroy());

  it('defaults to auto when the config has no verify section', async () => {
    await setup();
    expect(DEFAULT_BUILD_TEST_MODE).toBe('auto');
    expect(component.mode()).toBe('auto');
    expect(radio('auto').checked).toBeTrue();
    expect(radio('ask').checked).toBeFalse();
    expect(radio('off').checked).toBeFalse();
    expect(component.scope()).toBe('project');
    expect(component.projectOverride()).toBeNull();
    expect(component.pending()).toBeNull();
    expect(component.hasChanges()).toBeFalse();
  });

  it('renders a label and help text for every mode', async () => {
    await setup();
    const host = fixture.nativeElement as HTMLElement;
    const names = Array.from(host.querySelectorAll('.mode-name')).map((e) => e.textContent?.trim());
    expect(names.length).toBe(3);
    expect(names.every((n) => !!n)).toBeTrue();
    const helps = Array.from(host.querySelectorAll('.mode-help')).map((e) => e.textContent?.trim());
    expect(helps.length).toBe(3);
    expect(helps.every((h) => !!h)).toBeTrue();
  });

  it('reflects the configured mode and ignores garbage', async () => {
    await setup({ buildTest: 'ask' });
    expect(component.mode()).toBe('ask');
    expect(radio('ask').checked).toBeTrue();
    await setup({ buildTest: 'sometimes' });
    expect(component.mode()).toBe('auto');
  });

  it('stages a pending selection without saving', async () => {
    await setup();
    component.selectMode('off');
    expect(component.pending()).toBe('off');
    expect(component.hasChanges()).toBeTrue();
    expect(component.effectiveMode()).toBe('off');
    expect(radio('off').checked).toBeTrue();
    expect(engine.putConfig).not.toHaveBeenCalled();
  });

  it('save button is disabled when there are no changes', async () => {
    await setup();
    const btn = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>(
      '[data-testid="build-test-save"]',
    )!;
    expect(btn.disabled).toBeTrue();
  });

  it('save button is enabled when there are pending changes', async () => {
    await setup();
    component.selectMode('off');
    fixture.detectChanges();
    const btn = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>(
      '[data-testid="build-test-save"]',
    )!;
    expect(btn.disabled).toBeFalse();
  });

  it('save() persists the staged mode to the project config layer by default', async () => {
    await setup();
    component.selectMode('off');
    await component.save();
    expect(engine.putConfig).toHaveBeenCalledWith(
      'C:/tmp/project',
      { verify: { buildTest: 'off' } },
      { scope: 'project' },
    );
    expect(component.pending()).toBeNull();
    expect(component.store.saved()).toBeTruthy();
  });

  it('save() persists to the global layer when that scope is selected', async () => {
    await setup();
    component.setScope('global');
    component.selectMode('ask');
    await component.save();
    expect(engine.putConfig).toHaveBeenCalledWith(
      'C:/tmp/project',
      { verify: { buildTest: 'ask' } },
      { scope: 'global' },
    );
  });

  it('shows the project override when the project layer sets a value', async () => {
    await setup({ buildTest: 'off' }, '{ "verify": { "buildTest": "off" } }');
    expect(component.projectOverride()).toBe('off');
    const note = (fixture.nativeElement as HTMLElement).querySelector(
      '[data-testid="build-test-override"]',
    );
    expect(note).not.toBeNull();
    expect(note?.textContent).toContain('off');
  });

  it('keeps the radio on the effective mode when another layer overrides the save', async () => {
    await setup({ buildTest: 'off' }, '{ "verify": { "buildTest": "off" } }');
    // The engine keeps resolving `off` (project layer wins over the global save).
    (engine.putConfig as jasmine.Spy).and.returnValue(
      Promise.resolve(configResponse({ buildTest: 'off' })),
    );
    component.setScope('global');
    component.selectMode('auto');
    await component.save();
    fixture.detectChanges();
    expect(component.mode()).toBe('off');
    expect(radio('off').checked).toBeTrue();
    expect(radio('auto').checked).toBeFalse();
  });

  it('falls back to auto for an unknown value and reports engine errors', async () => {
    await setup({ buildTest: 'ask' });
    (engine.putConfig as jasmine.Spy).and.returnValue(Promise.reject(new Error('boom')));
    component.selectMode('nonsense');
    await component.save();
    expect((engine.putConfig as jasmine.Spy).calls.mostRecent().args[1]).toEqual({
      verify: { buildTest: 'auto' },
    });
    expect(component.store.error()).toContain('boom');
    expect(component.store.saving()).toBeFalse();
  });

  it('is a no-op while another save is running', async () => {
    await setup();
    component.selectMode('off');
    component.store.saving.set(true);
    await component.save();
    expect(engine.putConfig).not.toHaveBeenCalled();
  });

  it('scope change does not trigger hasChanges', async () => {
    await setup();
    expect(component.hasChanges()).toBeFalse();
    component.setScope('global');
    expect(component.hasChanges()).toBeFalse();
  });
});
