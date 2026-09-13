/**
 * Frontend verification card specs - WP-AUTOVERIFY (F8-1): the radio group
 * reads `config.verify.frontend`, saves `{ verify: { frontend } }` to the
 * chosen config layer (global by default) and reports a project override.
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
    expect(component.scope()).toBe('global');
    expect(component.projectOverride()).toBeNull();
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

  it('saves the chosen mode to the global config layer by default', async () => {
    await setup();
    await component.setMode('off');
    expect(engine.putConfig).toHaveBeenCalledWith(
      'C:/tmp/project',
      { verify: { frontend: 'off' } },
      { scope: 'global' },
    );
    expect(component.mode()).toBe('off');
    expect(component.store.saved()).toBeTruthy();
  });

  it('saves to the project layer when that scope is selected', async () => {
    await setup();
    component.setScope('project');
    await component.setMode('ask');
    expect(engine.putConfig).toHaveBeenCalledWith(
      'C:/tmp/project',
      { verify: { frontend: 'ask' } },
      { scope: 'project' },
    );
    component.setScope('nonsense');
    expect(component.scope()).toBe('global');
  });

  it('clicking a radio saves that mode', async () => {
    await setup();
    radio('ask').click();
    await fixture.whenStable();
    expect((engine.putConfig as jasmine.Spy).calls.mostRecent().args[1]).toEqual({
      verify: { frontend: 'ask' },
    });
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
    await component.setMode('nonsense');
    expect((engine.putConfig as jasmine.Spy).calls.mostRecent().args[1]).toEqual({
      verify: { frontend: 'auto' },
    });
    expect(component.store.error()).toContain('boom');
    expect(component.store.saving()).toBeFalse();
  });

  it('is a no-op while another save is running', async () => {
    await setup();
    component.store.saving.set(true);
    await component.setMode('off');
    expect(engine.putConfig).not.toHaveBeenCalled();
  });
});
