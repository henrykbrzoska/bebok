/**
 * WP-M2 / F10-8: desktop <-> mobile route twins and the guards that redirect
 * a deep link opened on the wrong form factor.
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { Router, provideRouter } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { FormFactor } from '../../core/form-factor';
import { desktopTwin, mobileTwin, redirectToDesktop, redirectToMobile } from './mobile.guards';

@Component({ selector: 'test-blank', template: '' })
class Blank {}

describe('route twins', () => {
  it('maps desktop paths to /m/**', () => {
    expect(mobileTwin('/')).toBe('/m/chat');
    expect(mobileTwin('')).toBe('/m/chat');
    expect(mobileTwin('/connect')).toBe('/m/chat');
    expect(mobileTwin('/chat/abc')).toBe('/m/chat/abc');
    expect(mobileTwin('/settings')).toBe('/m/more/settings');
    expect(mobileTwin('/config')).toBe('/m/more/settings');
    expect(mobileTwin('/stats')).toBe('/m/more/stats');
    expect(mobileTwin('/about')).toBe('/m/more/about');
    expect(mobileTwin('/terminal')).toBe('/m/more');
    expect(mobileTwin('/explorer?directory=x')).toBe('/m/more');
  });

  it('maps /m/** back to the flat desktop routes', () => {
    expect(desktopTwin('/m')).toBe('/');
    expect(desktopTwin('/m/chat')).toBe('/');
    expect(desktopTwin('/m/chat/abc')).toBe('/chat/abc');
    expect(desktopTwin('/m/remote')).toBe('/');
    expect(desktopTwin('/m/remote/abc')).toBe('/chat/abc');
    expect(desktopTwin('/m/agents')).toBe('/');
    expect(desktopTwin('/m/changes')).toBe('/');
    expect(desktopTwin('/m/more')).toBe('/');
    expect(desktopTwin('/m/more/settings')).toBe('/settings');
    expect(desktopTwin('/m/more/stats')).toBe('/stats');
    expect(desktopTwin('/m/more/about')).toBe('/about');
    expect(desktopTwin('/settings')).toBe('/settings');
  });
});

describe('form-factor route guards', () => {
  let router: Router;
  let formFactor: FormFactor;

  const testRoutes = [
    { path: '', component: Blank, canActivate: [redirectToMobile] },
    { path: 'chat/:sessionID', component: Blank, canActivate: [redirectToMobile] },
    { path: 'settings', component: Blank, canActivate: [redirectToMobile] },
    { path: 'stats', component: Blank, canActivate: [redirectToMobile] },
    {
      path: 'm',
      canMatch: [redirectToDesktop],
      children: [
        { path: 'chat', component: Blank },
        { path: 'chat/:sessionID', component: Blank },
        { path: 'more/settings', component: Blank },
        { path: 'more/stats', component: Blank },
      ],
    },
    { path: '**', redirectTo: '' },
  ];

  beforeEach(async () => {
    sessionStorage.clear();
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        provideRouter(testRoutes),
        { provide: EngineClient, useValue: { isTauri: () => false, isCapacitor: false } },
      ],
    });
    router = TestBed.inject(Router);
    formFactor = TestBed.inject(FormFactor);
  });

  afterEach(() => sessionStorage.clear());

  it('on a phone, desktop routes redirect to their /m twin (query kept)', async () => {
    formFactor.setOverride('mobile');
    await router.navigateByUrl('/');
    expect(router.url).toBe('/m/chat');
    await router.navigateByUrl('/chat/s1?directory=%2Fp');
    expect(router.url).toBe('/m/chat/s1?directory=%2Fp');
    await router.navigateByUrl('/settings');
    expect(router.url).toBe('/m/more/settings');
    await router.navigateByUrl('/stats');
    expect(router.url).toBe('/m/more/stats');
  });

  it('on a phone, /m routes are left alone', async () => {
    formFactor.setOverride('mobile');
    await router.navigateByUrl('/m/chat/s2');
    expect(router.url).toBe('/m/chat/s2');
  });

  it('on a desktop, /m routes redirect to the flat twin', async () => {
    formFactor.setOverride('desktop');
    await router.navigateByUrl('/m/chat');
    expect(router.url).toBe('/');
    await router.navigateByUrl('/m/chat/s3?directory=%2Fp');
    expect(router.url).toBe('/chat/s3?directory=%2Fp');
    await router.navigateByUrl('/m/more/settings');
    expect(router.url).toBe('/settings');
  });

  it('on a desktop, desktop routes are left alone', async () => {
    formFactor.setOverride('desktop');
    await router.navigateByUrl('/chat/s4');
    expect(router.url).toBe('/chat/s4');
  });
});
