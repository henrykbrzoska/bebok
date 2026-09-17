/**
 * General tab plugin-row specs (Plan B): the Update button is rendered for
 * every installed plugin, is the primary action while the engine reports a
 * missing platform binary (secondary otherwise), carries the available
 * version in its label, and `update()` posts to `POST /plugins/{name}/update`
 * and maps the engine's failure codes onto localised messages.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { I18nService } from '../../i18n/i18n.service';
import { ToastStore } from '../../ui/toast/toast.store';
import { GeneralTab, pluginUpdateErrorKey } from './general-tab';
import { SettingsStore } from './settings.store';

const REGISTRY = {
  plugins: [{ name: 'bebok-index', repo: 'o/r', url: 'https://example.test/r', description: 'desc' }],
};

function declared(extra: Record<string, unknown>): unknown {
  return {
    declared: [
      {
        name: 'bebok-index',
        repo: 'o/r',
        url: 'https://example.test/r',
        enabled: true,
        installed: true,
        ...extra,
      },
    ],
  };
}

describe('pluginUpdateErrorKey (Plan B)', () => {
  it('maps the engine failure codes onto i18n keys', () => {
    expect(pluginUpdateErrorKey('engine POST /plugins/x/update -> 503: {"error":"binary_missing"}'))
      .toBe('settings.pluginBinaryMissing');
    expect(pluginUpdateErrorKey('{"error":"no_asset_for_platform"}'))
      .toBe('settings.pluginNoAssetForPlatform');
    expect(pluginUpdateErrorKey('{"error":"offline_fallback"}'))
      .toBe('settings.pluginOfflineFallback');
    expect(pluginUpdateErrorKey('{"error":"checksum_mismatch"}'))
      .toBe('settings.pluginChecksumMismatch');
  });

  it('returns null for anything else', () => {
    expect(pluginUpdateErrorKey('engine POST … -> 500: boom')).toBeNull();
    expect(pluginUpdateErrorKey('')).toBeNull();
  });

  it('recognises the readable engine wording too', () => {
    expect(pluginUpdateErrorKey("plugin 'x' binary is missing — run Update in Settings")).toBe(
      'settings.pluginBinaryMissing',
    );
    expect(pluginUpdateErrorKey('503: {"error":"no_asset_for_platform"}')).toBe(
      'settings.pluginNoAssetForPlatform',
    );
  });
});

describe('GeneralTab plugin update button (Plan B)', () => {
  let fixture: ComponentFixture<GeneralTab>;
  let component: GeneralTab;
  let engine: EngineClient;
  let i18n: I18nService;
  let toasts: ToastStore;

  async function setup(opts: { declared?: unknown } = {}): Promise<void> {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [GeneralTab],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    i18n = TestBed.inject(I18nService);
    i18n.setLanguage('en');
    toasts = TestBed.inject(ToastStore);
    toasts.clear();
    spyOn(engine, 'connect').and.returnValue(Promise.resolve({ baseUrl: 'http://x' } as never));
    spyOn(engine, 'pluginRegistry').and.returnValue(Promise.resolve(REGISTRY as never));
    spyOn(engine, 'listPlugins').and.returnValue(
      Promise.resolve((opts.declared ?? { declared: [] }) as never),
    );
    spyOn(engine, 'getIndexStatus').and.returnValue(
      Promise.resolve({ status: 'ready', files: 1, symbols: 2 }),
    );
    fixture = TestBed.createComponent(GeneralTab);
    component = fixture.componentInstance;
    component.store.directory.set('C:/tmp/project');
    fixture.detectChanges();
    await component.refreshPlugins();
    fixture.detectChanges();
  }

  function row(name = 'bebok-index'): HTMLElement {
    return (fixture.nativeElement as HTMLElement).querySelector<HTMLElement>(
      `[data-testid="plugin-row-${name}"]`,
    )!;
  }

  function updateButton(name = 'bebok-index'): HTMLButtonElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector<HTMLButtonElement>(
      `[data-testid="plugin-update-${name}"]`,
    );
  }

  afterEach(() => fixture?.destroy());

  it('shows no Update button for a plugin that is not installed', async () => {
    await setup();
    expect(component.rows()[0].installed).toBeFalse();
    expect(updateButton()).toBeNull();
    expect(
      (fixture.nativeElement as HTMLElement).querySelector(
        '[data-testid="plugin-install-bebok-index"]',
      ),
    ).not.toBeNull();
  });

  it('renders a primary Update button plus the version when the binary is missing', async () => {
    await setup({ declared: declared({ version: '1.2.3', binary: 'missing' }) });
    const btn = updateButton()!;
    expect(btn).not.toBeNull();
    expect(btn.textContent?.trim()).toBe(`${i18n.t('settings.pluginUpdate')} v1.2.3`);
    expect(btn.classList.contains('primary')).toBeTrue();
    expect(btn.classList.contains('ghost')).toBeFalse();
    const warn = row().querySelector('[data-testid="plugin-binary-missing-bebok-index"]');
    expect(warn?.textContent?.trim()).toBe(i18n.t('settings.pluginBinaryMissing'));
  });

  it('renders a secondary Update button when the binary is present', async () => {
    await setup({ declared: declared({ version: '1.2.3', binary: 'present' }) });
    const btn = updateButton()!;
    expect(btn.textContent).toContain('v1.2.3');
    expect(btn.classList.contains('ghost')).toBeTrue();
    expect(btn.classList.contains('primary')).toBeFalse();
    expect(row().querySelector('[data-testid="plugin-binary-missing-bebok-index"]')).toBeNull();
  });

  it('omits the version from the label when the engine reports none', async () => {
    await setup({ declared: declared({}) });
    const btn = updateButton()!;
    expect(btn.textContent?.trim()).toBe(i18n.t('settings.pluginUpdate'));
    // No binary information at all: secondary styling, no warning.
    expect(btn.classList.contains('ghost')).toBeTrue();
    expect(btn.classList.contains('primary')).toBeFalse();
  });

  it('falls back to the registry version when the declaration has none', async () => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [GeneralTab],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    i18n = TestBed.inject(I18nService);
    i18n.setLanguage('en');
    spyOn(engine, 'connect').and.returnValue(Promise.resolve({ baseUrl: 'http://x' } as never));
    spyOn(engine, 'pluginRegistry').and.returnValue(
      Promise.resolve({ plugins: [{ ...REGISTRY.plugins[0], version: '2.0.0' }] } as never),
    );
    spyOn(engine, 'listPlugins').and.returnValue(
      Promise.resolve(declared({ binary: 'present' }) as never),
    );
    spyOn(engine, 'getIndexStatus').and.returnValue(Promise.resolve({ status: 'ready', files: 0, symbols: 0 }));
    fixture = TestBed.createComponent(GeneralTab);
    component = fixture.componentInstance;
    component.store.directory.set('C:/tmp/project');
    fixture.detectChanges();
    await component.refreshPlugins();
    fixture.detectChanges();
    expect(updateButton()?.textContent).toContain('v2.0.0');
  });

  it('update() posts to the engine, toasts and refreshes the list', async () => {
    await setup({ declared: declared({ version: '1.2.3', binary: 'missing' }) });
    const spy = spyOn(engine, 'updatePlugin').and.returnValue(
      Promise.resolve({ plugin: { name: 'bebok-index' } } as never),
    );
    const fetches = (engine.listPlugins as jasmine.Spy).calls.count();
    await component.update(component.rows()[0]);
    expect(spy).toHaveBeenCalledWith('C:/tmp/project', 'bebok-index');
    // The list is re-fetched so the row shows the fresh version/binary state.
    expect((engine.listPlugins as jasmine.Spy).calls.count()).toBe(fetches + 1);
    expect(toasts.toasts().length).toBe(1);
    expect(toasts.toasts()[0].text).toBe(i18n.t('settings.pluginUpdated', { name: 'bebok-index' }));
    expect(toasts.toasts()[0].kind).toBe('success');
    expect(component.busyName()).toBeNull();
  });

  it('update() maps binary_missing onto the localised message', async () => {
    await setup({ declared: declared({ version: '1.2.3', binary: 'missing' }) });
    spyOn(engine, 'updatePlugin').and.returnValue(
      Promise.reject(new Error('engine POST /plugins/bebok-index/update -> 503: {"error":"binary_missing"}')),
    );
    await component.update(component.rows()[0]);
    const toast = toasts.toasts()[toasts.toasts().length - 1];
    expect(toast.kind).toBe('danger');
    expect(toast.text).toBe(i18n.t('settings.pluginBinaryMissing'));
    expect(toast.text).not.toContain('engine POST');
    expect(component.busyName()).toBeNull();
  });

  it('update() treats an offline fallback as a warning', async () => {
    await setup({ declared: declared({ binary: 'missing' }) });
    spyOn(engine, 'updatePlugin').and.returnValue(
      Promise.reject(new Error('{"error":"offline_fallback"}')),
    );
    await component.update(component.rows()[0]);
    const toast = toasts.toasts()[toasts.toasts().length - 1];
    expect(toast.kind).toBe('warning');
    expect(toast.text).toBe(i18n.t('settings.pluginOfflineFallback'));
  });

  it('update() is a no-op while another row is busy or without a directory', async () => {
    await setup({ declared: declared({}) });
    const spy = spyOn(engine, 'updatePlugin').and.returnValue(
      Promise.resolve({ plugin: { name: 'bebok-index' } } as never),
    );
    component.busyName.set('other');
    await component.update(component.rows()[0]);
    expect(spy).not.toHaveBeenCalled();
    component.busyName.set(null);
    component.store.directory.set(null);
    await component.update(component.rows()[0]);
    expect(spy).not.toHaveBeenCalled();
  });
});

describe('GeneralTab index status refresh (404 -> unknown)', () => {
  let fixture: ComponentFixture<GeneralTab>;
  let component: GeneralTab;
  let engine: EngineClient;
  let i18n: I18nService;

  async function setupIndex(statusSpy: jasmine.Spy): Promise<void> {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [GeneralTab],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    i18n = TestBed.inject(I18nService);
    i18n.setLanguage('en');
    spyOn(engine, 'connect').and.returnValue(Promise.resolve({ baseUrl: 'http://x' } as never));
    spyOn(engine, 'pluginRegistry').and.returnValue(Promise.resolve(REGISTRY as never));
    spyOn(engine, 'listPlugins').and.returnValue(
      Promise.resolve({ declared: [] } as never),
    );
    (engine as unknown as Record<string, unknown>)['getIndexStatus'] = statusSpy;
    fixture = TestBed.createComponent(GeneralTab);
    component = fixture.componentInstance;
    component.store.directory.set('C:/tmp/project');
    fixture.detectChanges();
  }

  afterEach(() => fixture?.destroy());

  it('quiet poll shows unknown after 3 consecutive failures', async () => {
    const statusSpy = jasmine
      .createSpy('getIndexStatus')
      .and.returnValue(Promise.reject(new Error('engine GET /plugins/bebok-index/status -> 404')));
    await setupIndex(statusSpy);
    component.indexStatus.set({ status: 'ready', files: 1, symbols: 2 });
    await component.refreshIndexStatus(true);
    expect(component.indexStatus()?.status).toBe('ready');
    await component.refreshIndexStatus(true);
    expect(component.indexStatus()?.status).toBe('ready');
    await component.refreshIndexStatus(true);
    expect(component.indexStatus()?.status).toBe('unknown');
    expect(component.indexStatusText()).toBe(i18n.t('settings.indexUnknown'));
  });

  it('non-quiet refresh surfaces the 404 as indexError', async () => {
    const statusSpy = jasmine
      .createSpy('getIndexStatus')
      .and.returnValue(Promise.reject(new Error('engine GET /plugins/bebok-index/status -> 404')));
    await setupIndex(statusSpy);
    await component.refreshIndexStatus();
    expect(component.indexError()).toContain('404');
  });

  it('normalizeIndexStatus passes the rebuild flag through', async () => {
    const { normalizeIndexStatus } = await import('./general-tab');
    expect(normalizeIndexStatus({ status: 'ready', files: 1, symbols: 2, rebuild: true })).toEqual({
      status: 'ready',
      files: 1,
      symbols: 2,
      rebuild: true,
    });
    expect('rebuild' in normalizeIndexStatus({ status: 'ready', files: 1, symbols: 2 })).toBeFalse();
  });
});

describe('GeneralTab rebuild ok:false -> indexError', () => {
  let fixture: ComponentFixture<GeneralTab>;
  let component: GeneralTab;
  let engine: EngineClient;
  let i18n: I18nService;
  let toasts: ToastStore;

  async function setupRebuild(rebuildSpy: jasmine.Spy): Promise<void> {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [GeneralTab],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    i18n = TestBed.inject(I18nService);
    i18n.setLanguage('en');
    toasts = TestBed.inject(ToastStore);
    toasts.clear();
    spyOn(engine, 'connect').and.returnValue(Promise.resolve({ baseUrl: 'http://x' } as never));
    spyOn(engine, 'pluginRegistry').and.returnValue(Promise.resolve(REGISTRY as never));
    spyOn(engine, 'listPlugins').and.returnValue(
      Promise.resolve({ declared: [] } as never),
    );
    spyOn(engine, 'getIndexStatus').and.returnValue(
      Promise.resolve({ status: 'ready', files: 1, symbols: 2 }),
    );
    (engine as unknown as Record<string, unknown>)['rebuildIndex'] = rebuildSpy;
    fixture = TestBed.createComponent(GeneralTab);
    component = fixture.componentInstance;
    component.store.directory.set('C:/tmp/project');
    fixture.detectChanges();
  }

  afterEach(() => fixture?.destroy());

  it('does not toast success nor set indexLastRebuild when ok is false', async () => {
    const rebuildSpy = jasmine
      .createSpy('rebuildIndex')
      .and.returnValue(Promise.resolve({ status: 'error', files: 0, symbols: 0, rebuild: false, ok: false }));
    await setupRebuild(rebuildSpy);
    await component.rebuildIndex();
    expect(component.indexLastRebuild()).toBeNull();
    expect(component.indexError()).not.toBeNull();
    expect(toasts.toasts().length).toBe(1);
    expect(toasts.toasts()[0].kind).toBe('danger');
    expect(toasts.toasts()[0].text).not.toBe(i18n.t('settings.indexRebuilt'));
    expect(component.indexRebuilding()).toBeFalse();
  });

  it('clears indexError and toasts success when ok is not false', async () => {
    const rebuildSpy = jasmine
      .createSpy('rebuildIndex')
      .and.returnValue(Promise.resolve({ status: 'indexing', files: 0, symbols: 0, rebuild: true }));
    await setupRebuild(rebuildSpy);
    component.indexError.set('stale');
    await component.rebuildIndex();
    expect(component.indexError()).toBeNull();
    expect(component.indexLastRebuild()).not.toBeNull();
    const toast = toasts.toasts()[toasts.toasts().length - 1];
    expect(toast.kind).toBe('success');
    expect(toast.text).toBe(i18n.t('settings.indexRebuilt'));
  });

  it('sets indexError on a rejected rebuild', async () => {
    const rebuildSpy = jasmine
      .createSpy('rebuildIndex')
      .and.returnValue(Promise.reject(new Error('engine POST /plugins/bebok-index/rebuild -> 404')));
    await setupRebuild(rebuildSpy);
    await component.rebuildIndex();
    expect(component.indexError()).toContain('404');
    expect(component.indexLastRebuild()).toBeNull();
    expect(toasts.toasts()[toasts.toasts().length - 1].kind).toBe('danger');
  });
});

describe('GeneralTab updateLabel registry-target versions', () => {
  let fixture: ComponentFixture<GeneralTab>;
  let component: GeneralTab;
  let engine: EngineClient;
  let i18n: I18nService;

  async function setupVersions(registryVersion?: string, declaredVersion?: string): Promise<void> {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [GeneralTab],
      providers: [provideZonelessChangeDetection(), SettingsStore],
    });
    engine = TestBed.inject(EngineClient);
    i18n = TestBed.inject(I18nService);
    i18n.setLanguage('en');
    spyOn(engine, 'connect').and.returnValue(Promise.resolve({ baseUrl: 'http://x' } as never));
    spyOn(engine, 'pluginRegistry').and.returnValue(
      Promise.resolve({
        plugins: [{ ...REGISTRY.plugins[0], ...(registryVersion ? { version: registryVersion } : {}) }],
      } as never),
    );
    spyOn(engine, 'listPlugins').and.returnValue(
      Promise.resolve(
        (declaredVersion ? declared({ version: declaredVersion }) : declared({})) as never,
      ),
    );
    spyOn(engine, 'getIndexStatus').and.returnValue(
      Promise.resolve({ status: 'ready', files: 0, symbols: 0 }),
    );
    fixture = TestBed.createComponent(GeneralTab);
    component = fixture.componentInstance;
    component.store.directory.set('C:/tmp/project');
    fixture.detectChanges();
    await component.refreshPlugins();
    fixture.detectChanges();
  }

  afterEach(() => fixture?.destroy());

  it('shows bare Update when registry and installed versions agree', async () => {
    await setupVersions('1.2.3', '1.2.3');
    expect(component.updateLabel(component.rows()[0])).toBe(i18n.t('settings.pluginUpdate'));
  });

  it('shows Update vX with the registry version when they differ', async () => {
    await setupVersions('2.0.0', '1.2.3');
    expect(component.updateLabel(component.rows()[0])).toBe(
      `${i18n.t('settings.pluginUpdate')} v2.0.0`,
    );
  });
});
