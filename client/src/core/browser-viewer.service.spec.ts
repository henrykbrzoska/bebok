/**
 * WP-BROWSER2 (F7-6): opening the browser viewer window - a Tauri
 * `WebviewWindow` on the desktop, a `window.open` popup elsewhere.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import {
  BrowserViewerService,
  VIEWER_HEIGHT,
  VIEWER_WIDTH,
  viewerPath,
  viewerWindowFeatures,
  viewerWindowName,
} from './browser-viewer.service';
import { EngineClient } from './engine-client.service';

describe('BrowserViewerService helpers (F7-6)', () => {
  it('builds the shell-less viewer route with an encoded session id', () => {
    expect(viewerPath('abc-123')).toBe('/browser-view?session=abc-123');
    expect(viewerPath('a b&c')).toBe('/browser-view?session=a%20b%26c');
  });

  it('derives a stable, safe window name per session', () => {
    expect(viewerWindowName('11111111-2222')).toBe('bebok-browser-11111111-2222');
    expect(viewerWindowName('a/b:c')).toBe('bebok-browser-a_b_c');
    expect(viewerWindowName('x')).toBe(viewerWindowName('x'));
  });

  it('sizes the popup for a 1280x800 page but never larger than the screen', () => {
    expect(viewerWindowFeatures({})).toContain(`width=${VIEWER_WIDTH},height=${VIEWER_HEIGHT}`);
    expect(viewerWindowFeatures({ availWidth: 1000, availHeight: 600 })).toContain(
      'width=1000,height=600',
    );
    expect(viewerWindowFeatures({})).toContain('popup=yes');
  });
});

describe('BrowserViewerService (F7-6)', () => {
  let service: BrowserViewerService;
  let isTauri: ReturnType<typeof signal<boolean>>;

  beforeEach(() => {
    isTauri = signal(false);
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: { isTauri } },
      ],
    });
    service = TestBed.inject(BrowserViewerService);
  });

  it('opens a named popup in browser mode and focuses it', async () => {
    const popup = { focus: jasmine.createSpy('focus') } as unknown as Window;
    const open = spyOn(window, 'open').and.returnValue(popup);
    expect(await service.open('s1')).toBeTrue();
    expect(open).toHaveBeenCalledTimes(1);
    const [url, name, features] = open.calls.mostRecent().args;
    expect(String(url)).toBe(`${window.location.origin}/browser-view?session=s1`);
    expect(name).toBe('bebok-browser-s1');
    expect(String(features)).toContain('popup=yes');
    expect((popup as unknown as { focus: jasmine.Spy }).focus).toHaveBeenCalled();
  });

  it('reports a blocked popup', async () => {
    spyOn(window, 'open').and.returnValue(null);
    expect(await service.open('s1')).toBeFalse();
  });

  it('falls back to a popup when the Tauri command is unavailable', async () => {
    // Under Karma there is no `__TAURI_INTERNALS__`, so the dynamic import of
    // `@tauri-apps/api/core` resolves but `invoke` rejects; the service must
    // degrade to `window.open` instead of failing.
    isTauri.set(true);
    const open = spyOn(window, 'open').and.returnValue({
      focus: () => undefined,
    } as unknown as Window);
    spyOn(console, 'error');
    expect(await service.open('s2')).toBeTrue();
    expect(open).toHaveBeenCalled();
  });
});
