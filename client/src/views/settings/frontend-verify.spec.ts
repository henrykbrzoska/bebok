/**
 * Frontend verification card specs - WP-AUTOVERIFY (F8-1): the radio group
 * stages a pending selection, and an explicit Save button persists
 * `{ verify: { frontend } }` to the chosen config layer (project by default).
 * A project override is reported when the project layer sets its own value.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { ConfigResponse } from '../../core/engine.dtos';
import {
  DEFAULT_FRONTEND_VERIFY,
  FrontendVerifyCard,
  layerFrontendVerify,
} from './frontend-verify';
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

describe('layerFrontendVerify (F8-1)', () => {
  it('reads verify.frontend out of a raw layer and ignores junk', () => {
    expect(layerFrontendVerify('{ "verify": { "frontend": "ask" } }')).toBe('ask');
    expect(layerFrontendVerify('{ "verify": { "frontend": "loud" } }')).toBeNull();
    expect(layerFrontendVerify('{}')).toBeNull();
    expect(layerFrontendVerify('not json')).toBeNull();
    expect(layerFrontendVerify(undefined)).toBeNull();
  });
});

describe('FrontendVerifyCard (F8-1)', () => {
  let fixture: ComponentFixture<FrontendVerifyCard>;
  let component: FrontendVerifyCard;
  let engine: EngineClient;

  async function setup(verify?: Record<string, unknown>, projectContent = '{}'): Promise<void> {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [FrontendVerifyCard],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    spyOn(engine, 'getConfig').and.returnValue(
      Promise.resolve(configResponse(verify, projectContent)),
    );
    spyOn(engine, 'putConfig').and.callFake((_dir: string, delta: unknown) =>
      Promise.resolve(configResponse((delta as { verify: Record<string, unknown> }).verify)),
    );
    fixture = TestBed.createComponent(FrontendVerifyCard);
    component = fixture.componentInstance;
    await component.store.load('C:/tmp/project');
    fixture.detectChanges();
  }

  function radio(mode: string): HTMLInputElement {
    return (fixture.nativeElement as HTMLElement).querySelector<HTMLInputElement>(
      `[data-testid="frontend-verify-${mode}"]`,
    )!;
  }

  afterEach(() => fixture?.destroy());

  it('defaults to auto when the config has no verify section', async () => {
    await setup();
    expect(DEFAULT_FRONTEND_VERIFY).toBe('auto');
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
    // The permission implication of `auto` is documented on the card.
    expect(host.querySelector('.perm-note')?.textContent).toContain('browser_eval');
  });

  it('reflects the configured mode and ignores garbage', async () => {
    await setup({ frontend: 'ask' });
    expect(component.mode()).toBe('ask');
    expect(radio('ask').checked).toBeTrue();
    await setup({ frontend: 'sometimes' });
    expect(component.mode()).toBe('auto');
  });

  it('stages a pending selection without saving', async () => {
    await setup();
    component.selectMode('off');
    expect(component.pending()).toBe('off');
    expect(component.hasChanges()).toBeTrue();
    expect(component.effectiveMode()).toBe('off');
    // Radio reflects the pending selection.
    expect(radio('off').checked).toBeTrue();
    // No PUT was made.
    expect(engine.putConfig).not.toHaveBeenCalled();
  });

  it('save button is disabled when there are no changes', async () => {
    await setup();
    const btn = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>(
      '[data-testid="frontend-verify-save"]',
    )!;
    expect(btn.disabled).toBeTrue();
  });

  it('save button is enabled when there are pending changes', async () => {
    await setup();
    component.selectMode('off');
    fixture.detectChanges();
    const btn = (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>(
      '[data-testid="frontend-verify-save"]',
    )!;
    expect(btn.disabled).toBeFalse();
  });

  it('save() persists the staged mode to the project config layer by default', async () => {
    await setup();
    component.selectMode('off');
    await component.save();
    expect(engine.putConfig).toHaveBeenCalledWith(
      'C:/tmp/project',
      { verify: { frontend: 'off' } },
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
      { verify: { frontend: 'ask' } },
      { scope: 'global' },
    );
  });

  it('selecting a different radio and saving stages correctly', async () => {
    await setup();
    component.selectMode('ask');
    await component.save();
    expect((engine.putConfig as jasmine.Spy).calls.mostRecent().args[1]).toEqual({
      verify: { frontend: 'ask' },
    });
    // pending reset after save
    expect(component.pending()).toBeNull();
  });

  it('shows the project override when the project layer sets a value', async () => {
    await setup({ frontend: 'off' }, '{ "verify": { "frontend": "off" } }');
    expect(component.projectOverride()).toBe('off');
    const note = (fixture.nativeElement as HTMLElement).querySelector(
      '[data-testid="frontend-verify-override"]',
    );
    expect(note).not.toBeNull();
    expect(note?.textContent).toContain('off');
  });

  it('falls back to auto for an unknown value and reports engine errors', async () => {
    await setup({ frontend: 'ask' });
    (engine.putConfig as jasmine.Spy).and.returnValue(Promise.reject(new Error('boom')));
    component.selectMode('nonsense');
    await component.save();
    expect((engine.putConfig as jasmine.Spy).calls.mostRecent().args[1]).toEqual({
      verify: { frontend: 'auto' },
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
