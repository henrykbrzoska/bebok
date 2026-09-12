/**
 * F1-3: `activeScreen` must be *derived from the router* (route `data.screen`),
 * so deep links keep working and there is no parallel hand-rolled signal.
 */

import { Component } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection } from '@angular/core';
import { Router, provideRouter } from '@angular/router';

import { ShellStore } from './shell.store';

@Component({ selector: 'test-blank', template: '' })
class Blank {}

const testRoutes = [
  { path: '', component: Blank, data: { screen: 'start' } },
  { path: 'chat/:sessionID', component: Blank, data: { screen: 'chat' } },
  { path: 'settings', component: Blank, data: { screen: 'settings' } },
  { path: 'explorer', component: Blank, data: { screen: 'explorer' } },
  { path: 'config', redirectTo: 'settings', pathMatch: 'full' as const },
];

describe('ShellStore', () => {
  let router: Router;
  let store: ShellStore;

  beforeEach(async () => {
    localStorage.clear();
    TestBed.configureTestingModule({
      providers: [provideZonelessChangeDetection(), provideRouter(testRoutes)],
    });
    router = TestBed.inject(Router);
    store = TestBed.inject(ShellStore);
    await router.navigateByUrl('/');
  });

  it('derives activeScreen from route data', async () => {
    expect(store.activeScreen()).toBe('start');

    await router.navigateByUrl('/explorer');
    expect(store.activeScreen()).toBe('explorer');

    await router.navigateByUrl('/settings');
    expect(store.activeScreen()).toBe('settings');
    expect(store.isChat()).toBeFalse();
  });

  it('exposes the chat session id from the route params', async () => {
    await router.navigateByUrl('/chat/abc123');
    expect(store.activeScreen()).toBe('chat');
    expect(store.isChat()).toBeTrue();
    expect(store.currentSessionId()).toBe('abc123');

    await router.navigateByUrl('/settings');
    expect(store.currentSessionId()).toBeNull();
  });

  it('follows the /config -> /settings redirect', async () => {
    await router.navigateByUrl('/config');
    expect(store.activeScreen()).toBe('settings');
  });

  it('owns the shell layout signals', () => {
    expect(store.sidebarExpanded()).toBeTrue();
    store.toggleSidebar();
    expect(store.sidebarExpanded()).toBeFalse();

    expect(store.commandPaletteOpen()).toBeFalse();
    store.openCommandPalette();
    expect(store.commandPaletteOpen()).toBeTrue();
    store.closeCommandPalette();
    expect(store.commandPaletteOpen()).toBeFalse();

    expect(store.density()).toBe('comfortable');
    store.toggleDensity();
    expect(store.density()).toBe('compact');

    expect(store.rightDrawerPanels()).toEqual({
      session: true,
      explorer: true,
      terminal: false,
      agents: false,
      changes: false,
      preview: false,
      browser: false,
    });
    store.toggleRightDrawerPanel('terminal');
    expect(store.rightDrawerPanels().terminal).toBeTrue();
    expect(store.rightDrawerPanels().session).toBeTrue();
  });
});
