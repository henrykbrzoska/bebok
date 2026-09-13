/**
 * WP-M2 / F10-8: `FormFactor.isMobile` = Capacitor || narrow viewport ||
 * `?ff=mobile` (remembered for the tab in sessionStorage).
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';

import { EngineClient } from './engine-client.service';
import {
  FORM_FACTOR_PARAM,
  FORM_FACTOR_SESSION_KEY,
  FormFactor,
  readFormFactorOverride,
} from './form-factor';

describe('readFormFactorOverride', () => {
  it('accepts mobile/desktop and rejects everything else', () => {
    expect(readFormFactorOverride('?ff=mobile')).toBe('mobile');
    expect(readFormFactorOverride('?x=1&ff=desktop')).toBe('desktop');
    expect(readFormFactorOverride('?ff=tablet')).toBeNull();
    expect(readFormFactorOverride('?ff=')).toBeNull();
    expect(readFormFactorOverride('')).toBeNull();
  });
});

describe('FormFactor', () => {
  const originalHref = window.location.href;
  let matches = false;
  let listeners: Array<(ev: MediaQueryListEvent) => void>;
  let engine: { isTauri: () => boolean; isCapacitor: boolean };

  function loadPageWith(search: string): void {
    const url = new URL(originalHref);
    url.search = search;
    window.history.replaceState(null, '', url.toString());
  }

  function make(): FormFactor {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      providers: [provideZonelessChangeDetection(), { provide: EngineClient, useValue: engine }],
    });
    return TestBed.inject(FormFactor);
  }

  beforeEach(() => {
    sessionStorage.clear();
    listeners = [];
    matches = false;
    engine = { isTauri: () => false, isCapacitor: false };
    spyOn(window, 'matchMedia').and.callFake(
      (query: string) =>
        ({
          matches,
          media: query,
          addEventListener: (_type: string, cb: (ev: MediaQueryListEvent) => void) =>
            listeners.push(cb),
          removeEventListener: () => undefined,
        }) as unknown as MediaQueryList,
    );
  });

  afterEach(() => {
    window.history.replaceState(null, '', originalHref);
    sessionStorage.clear();
  });

  it('is desktop by default', () => {
    loadPageWith('');
    expect(make().isMobile()).toBeFalse();
  });

  it('is mobile on the Capacitor shell', () => {
    loadPageWith('');
    engine.isCapacitor = true;
    expect(make().isMobile()).toBeTrue();
  });

  it('follows the (max-width: 767px) media query, but never in Tauri', () => {
    loadPageWith('');
    matches = true;
    expect(make().isMobile()).toBeTrue();

    engine.isTauri = () => true;
    expect(make().isMobile()).toBeFalse();
  });

  it('reacts to media query changes', () => {
    loadPageWith('');
    const ff = make();
    expect(ff.isMobile()).toBeFalse();
    for (const cb of listeners) {
      cb({ matches: true } as MediaQueryListEvent);
    }
    expect(ff.isMobile()).toBeTrue();
  });

  it('?ff=mobile forces the phone shell and is remembered for the tab', () => {
    loadPageWith(`?${FORM_FACTOR_PARAM}=mobile`);
    expect(make().isMobile()).toBeTrue();
    expect(sessionStorage.getItem(FORM_FACTOR_SESSION_KEY)).toBe('mobile');

    // Next in-app load without the param (the router rewrote the URL).
    loadPageWith('');
    expect(make().isMobile()).toBeTrue();
  });

  it('?ff=desktop overrides a narrow viewport and clears a remembered mobile', () => {
    sessionStorage.setItem(FORM_FACTOR_SESSION_KEY, 'mobile');
    matches = true;
    loadPageWith(`?${FORM_FACTOR_PARAM}=desktop`);
    expect(make().isMobile()).toBeFalse();
    expect(sessionStorage.getItem(FORM_FACTOR_SESSION_KEY)).toBe('desktop');
  });

  it('setOverride(null) returns to the detected value', () => {
    loadPageWith(`?${FORM_FACTOR_PARAM}=mobile`);
    const ff = make();
    expect(ff.isMobile()).toBeTrue();
    ff.setOverride(null);
    expect(ff.isMobile()).toBeFalse();
    expect(sessionStorage.getItem(FORM_FACTOR_SESSION_KEY)).toBeNull();
  });
});
