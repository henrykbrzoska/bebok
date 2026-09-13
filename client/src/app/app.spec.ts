/**
 * WP-BROWSER2 (F7-6): routes flagged `data.bare` (the browser viewer window)
 * render without the shell.
 */

import { ActivatedRouteSnapshot } from '@angular/router';

import { isBarePath, isBareRoute } from './app';

function snapshot(chain: Array<Record<string, unknown>>): ActivatedRouteSnapshot {
  // Build a root -> child -> grandchild chain carrying `data` only.
  let child: ActivatedRouteSnapshot | null = null;
  for (let i = chain.length - 1; i >= 0; i--) {
    const node = { data: chain[i], firstChild: child } as unknown as ActivatedRouteSnapshot;
    child = node;
  }
  return child!;
}

describe('App bare-route detection (F7-6)', () => {
  it('reads data.bare from the deepest activated route', () => {
    expect(isBareRoute(snapshot([{}, { screen: 'browserView', bare: true }]))).toBeTrue();
    expect(isBareRoute(snapshot([{}, { screen: 'chat' }]))).toBeFalse();
    expect(isBareRoute(snapshot([{ bare: true }, { screen: 'chat' }]))).toBeFalse();
    expect(isBareRoute(snapshot([{}]))).toBeFalse();
  });

  it('guesses the viewer route from the initial pathname', () => {
    expect(isBarePath('/browser-view')).toBeTrue();
    expect(isBarePath('/browser-view/')).toBeTrue();
    expect(isBarePath('/browser-viewer')).toBeFalse();
    expect(isBarePath('/chat/abc')).toBeFalse();
    expect(isBarePath('')).toBeFalse();
  });
});
