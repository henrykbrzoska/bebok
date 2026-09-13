/**
 * F9-2: right-drawer layout preferences - 320px default / 280px floor,
 * per-panel collapsed state persisted to localStorage, and the programmatic
 * "reveal" request that opens the drawer + panel, expands it and bumps a
 * nonce for the drawer to scroll on.
 */

import { TestBed } from '@angular/core/testing';

import {
  DEFAULT_DRAWER_WIDTH,
  MAX_DRAWER_WIDTH,
  MIN_DRAWER_WIDTH,
  UiPrefsStore,
} from './ui-prefs.store';

describe('UiPrefsStore (F9-2 drawer collapse + reveal)', () => {
  let prefs: UiPrefsStore;

  beforeEach(() => {
    localStorage.clear();
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({});
    prefs = TestBed.inject(UiPrefsStore);
  });

  it('defaults the drawer to 320px and clamps resizes to 280..560', () => {
    expect(DEFAULT_DRAWER_WIDTH).toBe(320);
    expect(MIN_DRAWER_WIDTH).toBe(280);
    expect(MAX_DRAWER_WIDTH).toBe(560);
    expect(prefs.rightDrawerWidth()).toBe(320);
    prefs.setRightDrawerWidth(100);
    expect(prefs.rightDrawerWidth()).toBe(280);
    prefs.setRightDrawerWidth(9000);
    expect(prefs.rightDrawerWidth()).toBe(560);
  });

  it('starts with every panel expanded and persists a collapse per panel', () => {
    expect(prefs.rightDrawerCollapsed().session).toBeFalse();
    prefs.toggleRightDrawerPanelCollapsed('session');
    expect(prefs.rightDrawerCollapsed().session).toBeTrue();
    expect(prefs.rightDrawerCollapsed().explorer).toBeFalse();
    const stored = JSON.parse(localStorage.getItem('bebok.ui.shell.rightDrawerCollapsed') ?? '{}');
    expect(stored.session).toBeTrue();

    // A fresh store instance reads the persisted state back.
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({});
    const again = TestBed.inject(UiPrefsStore);
    expect(again.rightDrawerCollapsed().session).toBeTrue();
  });

  it('reveal opens the drawer, turns the pill on, expands the panel and bumps the nonce', () => {
    prefs.setRightDrawerOpen(false);
    prefs.setRightDrawerPanel('preview', false);
    prefs.setRightDrawerPanelCollapsed('preview', true);
    expect(prefs.rightDrawerReveal()).toBeNull();

    prefs.revealRightDrawerPanel('preview');

    expect(prefs.rightDrawerOpen()).toBeTrue();
    expect(prefs.rightDrawerPanels().preview).toBeTrue();
    expect(prefs.rightDrawerCollapsed().preview).toBeFalse();
    const first = prefs.rightDrawerReveal();
    expect(first?.panel).toBe('preview');
    expect(first?.nonce).toBe(1);

    // Same panel again: a new nonce so the drawer scrolls again.
    prefs.revealRightDrawerPanel('preview');
    expect(prefs.rightDrawerReveal()?.nonce).toBe(2);
  });

  it('an agent-driven reveal (openDrawer=false) never re-opens a closed drawer', () => {
    prefs.setRightDrawerOpen(false);
    prefs.revealRightDrawerPanel('agents', false);
    expect(prefs.rightDrawerOpen()).toBeFalse();
    expect(prefs.rightDrawerPanels().agents).toBeTrue();
    expect(prefs.rightDrawerReveal()?.panel).toBe('agents');
  });

  it('setRightDrawerPanel is idempotent (no spurious writes)', () => {
    const before = localStorage.getItem('bebok.ui.shell.rightDrawerPanels');
    prefs.setRightDrawerPanel('session', prefs.rightDrawerPanels().session);
    expect(localStorage.getItem('bebok.ui.shell.rightDrawerPanels')).toBe(before);
  });
});
