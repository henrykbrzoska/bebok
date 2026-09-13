/**
 * F9-2 / F9-3: the right drawer's collapsible sticky sections and the
 * programmatic reveal. Only the Preview section is opened (its panel has no
 * network side effects until a file is requested), which is enough to
 * exercise the header toggle, the ✕, the keyboard path and the scroll.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { ExplorerSelectionStore } from '../../core/explorer-selection.store';
import { UiPrefsStore } from '../../core/ui-prefs.store';
import { RightDrawer } from './right-drawer';

describe('RightDrawer (F9-2 collapse / sticky headers / reveal)', () => {
  let fixture: ComponentFixture<RightDrawer>;
  let prefs: UiPrefsStore;
  let host: HTMLElement;

  beforeEach(() => {
    localStorage.clear();
    localStorage.setItem(
      'bebok.ui.shell.rightDrawerPanels',
      JSON.stringify({
        session: false,
        explorer: false,
        terminal: false,
        agents: false,
        changes: false,
        preview: true,
        browser: false,
      }),
    );
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [RightDrawer],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    const engine = TestBed.inject(EngineClient);
    spyOn(engine, 'fsFile').and.resolveTo({ path: 'docs/a.md', content: '# A' });
    prefs = TestBed.inject(UiPrefsStore);
    fixture = TestBed.createComponent(RightDrawer);
    host = fixture.nativeElement as HTMLElement;
    fixture.detectChanges();
  });

  function section(id: string): HTMLElement {
    return host.querySelector(`[data-panel="${id}"]`) as HTMLElement;
  }

  it('renders one scroll container with a sticky header per open section', () => {
    const panels = host.querySelector('[data-testid="drawer-panels"]') as HTMLElement;
    expect(panels).toBeTruthy();
    expect(getComputedStyle(panels).overflowY).toBe('auto');
    const head = section('preview').querySelector('.panel-head') as HTMLElement;
    expect(getComputedStyle(head).position).toBe('sticky');
    expect(host.querySelectorAll('.panel').length).toBe(1);
  });

  it('header click collapses the body (kept alive, hidden) and remembers it', async () => {
    const toggle = section('preview').querySelector('.panel-toggle') as HTMLButtonElement;
    expect(toggle.getAttribute('aria-expanded')).toBe('true');
    toggle.click();
    await fixture.whenStable();
    fixture.detectChanges();
    const body = section('preview').querySelector(':scope > .section-body') as HTMLElement;
    expect(body.hidden).toBeTrue();
    expect(body.querySelector('app-preview-panel')).toBeTruthy(); // not destroyed
    expect(toggle.getAttribute('aria-expanded')).toBe('false');
    expect(prefs.rightDrawerCollapsed().preview).toBeTrue();
    toggle.click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(body.hidden).toBeFalse();
  });

  it('✕ closes the section without touching its collapsed state', async () => {
    const close = section('preview').querySelector('.panel-close') as HTMLButtonElement;
    close.click();
    await fixture.whenStable();
    fixture.detectChanges();
    expect(section('preview')).toBeNull();
    expect(prefs.rightDrawerPanels().preview).toBeFalse();
    expect(prefs.rightDrawerCollapsed().preview).toBeFalse();
  });

  it('a reveal expands the panel and scrolls the drawer container (never the document)', async () => {
    prefs.setRightDrawerPanelCollapsed('preview', true);
    await fixture.whenStable();
    fixture.detectChanges();
    const panels = host.querySelector('[data-testid="drawer-panels"]') as HTMLElement;
    const scrollTo = spyOn(panels, 'scrollTo');
    const docScrollTo = spyOn(window, 'scrollTo');

    TestBed.inject(ExplorerSelectionStore).openInPreview('/proj', 'docs/a.md');
    await fixture.whenStable();
    fixture.detectChanges();
    await fixture.whenStable();

    expect(prefs.rightDrawerCollapsed().preview).toBeFalse();
    expect(section('preview').getAttribute('data-collapsed')).toBe('false');
    expect(scrollTo).toHaveBeenCalled();
    expect(docScrollTo).not.toHaveBeenCalled();
  });
});
