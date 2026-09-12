/**
 * WP-CHANGES (F6-9): the diff overlay fetches the engine diff, reverts only
 * after confirmation, and hands off to the Explorer through the shared
 * selection store + `/explorer?directory=` navigation.
 */

import { Component, provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { ExplorerSelectionStore } from '../right-drawer/panels/explorer-selection.store';
import { DiffOverlay } from './diff-overlay';

@Component({
  imports: [DiffOverlay],
  template: `
    <app-diff-overlay
      [sessionId]="sessionId()"
      [path]="path()"
      [directory]="directory()"
      (closed)="closed = closed + 1"
      (reverted)="reverted.push($event)"
    />
  `,
})
class Host {
  readonly sessionId = signal('s1');
  readonly path = signal('src/a.ts');
  readonly directory = signal<string | null>('/p');
  closed = 0;
  reverted: string[] = [];
}

describe('DiffOverlay (F6-9)', () => {
  let fixture: ComponentFixture<Host>;
  let engine: { sessionChangeDiff: jasmine.Spy; revertSessionChange: jasmine.Spy };
  let navigate: jasmine.Spy;
  let confirmSpy: jasmine.Spy;

  beforeEach(() => {
    localStorage.clear();
    engine = {
      sessionChangeDiff: jasmine.createSpy('sessionChangeDiff').and.resolveTo({
        path: 'src/a.ts',
        diff: '--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1,2 +1,2 @@\n-old\n+new\n keep',
        baseline: 'git',
        added: 1,
        removed: 1,
      }),
      revertSessionChange: jasmine.createSpy('revertSessionChange').and.resolveTo({
        path: 'src/a.ts',
        baseline: 'git',
        exists: true,
      }),
    };
    TestBed.configureTestingModule({
      imports: [Host],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: engine },
      ],
    });
    navigate = spyOn(TestBed.inject(Router), 'navigate').and.resolveTo(true);
    confirmSpy = spyOn(window, 'confirm').and.returnValue(true);
    fixture = TestBed.createComponent(Host);
    fixture.detectChanges();
  });

  afterEach(() => fixture.destroy());

  async function settle(): Promise<void> {
    await fixture.whenStable();
    await Promise.resolve();
    fixture.detectChanges();
  }

  function el(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  function button(testId: string): HTMLButtonElement {
    return el().querySelector(`[data-testid="${testId}"]`) as HTMLButtonElement;
  }

  it('fetches and renders the unified diff with the baseline badge', async () => {
    await settle();
    expect(engine.sessionChangeDiff).toHaveBeenCalledWith('s1', 'src/a.ts');
    expect(el().querySelector('app-diff-view')).not.toBeNull();
    expect(el().textContent).toContain('vs git HEAD');
    expect(el().textContent).toContain('src/a.ts');
  });

  it('reverts only after confirmation, then reports and closes', async () => {
    await settle();
    confirmSpy.and.returnValue(false);
    button('revert-file').click();
    await settle();
    expect(confirmSpy).toHaveBeenCalled();
    expect(engine.revertSessionChange).not.toHaveBeenCalled();
    expect(fixture.componentInstance.closed).toBe(0);

    confirmSpy.and.returnValue(true);
    button('revert-file').click();
    await settle();
    expect(engine.revertSessionChange).toHaveBeenCalledWith('s1', 'src/a.ts');
    expect(fixture.componentInstance.reverted).toEqual(['src/a.ts']);
    expect(fixture.componentInstance.closed).toBe(1);
  });

  it('selects the file and navigates to the Explorer', async () => {
    await settle();
    button('open-explorer').click();
    await settle();
    const selection = TestBed.inject(ExplorerSelectionStore);
    expect(selection.directory()).toBe('/p');
    expect(selection.selectedPath()).toBe('src/a.ts');
    expect(navigate).toHaveBeenCalledWith(['/explorer'], { queryParams: { directory: '/p' } });
    expect(fixture.componentInstance.closed).toBe(1);
  });

  it('closes on Escape and on a backdrop click', async () => {
    await settle();
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    await settle();
    expect(fixture.componentInstance.closed).toBe(1);
    (el().querySelector('.backdrop') as HTMLElement).click();
    await settle();
    expect(fixture.componentInstance.closed).toBe(2);
    // Clicks inside the card do not close.
    (el().querySelector('.card') as HTMLElement).click();
    await settle();
    expect(fixture.componentInstance.closed).toBe(2);
  });

  it('surfaces a failed diff fetch instead of an empty card', async () => {
    engine.sessionChangeDiff.and.rejectWith(new Error('engine GET -> 400: no tracked change'));
    fixture.componentInstance.path.set('other.ts');
    await settle();
    expect(el().textContent).toContain('no tracked change');
    expect(el().querySelector('app-diff-view')).toBeNull();
  });
});
