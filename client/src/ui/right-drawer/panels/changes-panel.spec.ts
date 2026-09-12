/**
 * WP-CHANGES (F6-9): the Changes panel lists engine-tracked files from
 * `GET /session/{id}/changes` and opens the diff overlay on a row click.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { By } from '@angular/platform-browser';
import { provideRouter } from '@angular/router';

import { EngineClient } from '../../../core/engine-client.service';
import { ChangeEntry, SessionMeta } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { DiffOverlay } from '../../diff-overlay/diff-overlay';
import { ChangesPanel } from './changes-panel';

const META: SessionMeta = {
  id: 's1',
  directory: '/p',
  agent: 'code',
  created_at: 0,
  updated_at: 0,
  usage: { input_tokens: 0, output_tokens: 0 },
} as SessionMeta;

const CHANGES: ChangeEntry[] = [
  { path: 'src/a.ts', added: 3, removed: 1, baseline: 'git', exists: true },
  { path: 'docs/new.md', added: 12, removed: 0, baseline: 'snapshot', exists: true },
];

describe('ChangesPanel (F6-9)', () => {
  let fixture: ComponentFixture<ChangesPanel>;
  let engine: {
    connected: ReturnType<typeof signal<boolean>>;
    sessionChanges: jasmine.Spy;
    sessionChangeDiff: jasmine.Spy;
    revertSessionChange: jasmine.Spy;
  };
  let session: ChatSessionStore;

  beforeEach(() => {
    localStorage.clear();
    engine = {
      connected: signal(true),
      sessionChanges: jasmine.createSpy('sessionChanges').and.resolveTo(CHANGES),
      sessionChangeDiff: jasmine.createSpy('sessionChangeDiff').and.resolveTo({
        path: 'src/a.ts',
        diff: '--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1,1 +1,1 @@\n-old\n+new',
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
    const events = { onEvent: jasmine.createSpy('onEvent').and.returnValue(() => undefined) };
    TestBed.configureTestingModule({
      imports: [ChangesPanel],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: engine },
        { provide: EventsStore, useValue: events },
      ],
    });
    session = TestBed.inject(ChatSessionStore);
    fixture = TestBed.createComponent(ChangesPanel);
    fixture.detectChanges();
  });

  afterEach(() => fixture.destroy());

  async function settle(): Promise<void> {
    await fixture.whenStable();
    await Promise.resolve();
    fixture.detectChanges();
  }

  function rows(): HTMLElement[] {
    return Array.from(
      (fixture.nativeElement as HTMLElement).querySelectorAll('[data-testid="change-row"]'),
    );
  }

  it('shows the no-session hint and does not fetch without a session', () => {
    const el = fixture.nativeElement as HTMLElement;
    expect(el.textContent).toContain('Open a session');
    expect(engine.sessionChanges).not.toHaveBeenCalled();
  });

  it('lists tracked files with +/- counts for the open session', async () => {
    session.meta.set(META);
    await settle();
    expect(engine.sessionChanges).toHaveBeenCalledWith('s1');
    const list = rows();
    expect(list.length).toBe(2);
    expect(list[0].textContent).toContain('src/a.ts');
    expect(list[0].textContent).toContain('+3');
    expect(list[0].textContent).toContain('-1');
    expect(list[1].textContent).toContain('docs/new.md');
    expect(list[1].textContent).toContain('+12');
    expect((fixture.nativeElement as HTMLElement).textContent).toContain('Tracked files: 2');
  });

  it('opens the diff overlay for the clicked row', async () => {
    session.meta.set(META);
    await settle();
    rows()[0].click();
    await settle();
    const el = fixture.nativeElement as HTMLElement;
    expect(fixture.componentInstance.openPath()).toBe('src/a.ts');
    expect(el.querySelector('app-diff-overlay')).not.toBeNull();
    expect(engine.sessionChangeDiff).toHaveBeenCalledWith('s1', 'src/a.ts');
  });

  it('re-lists when the turn settles', async () => {
    session.meta.set(META);
    await settle();
    expect(engine.sessionChanges).toHaveBeenCalledTimes(1);
    session.running.set(true);
    await settle();
    session.running.set(false);
    await settle();
    expect(engine.sessionChanges.calls.count()).toBeGreaterThanOrEqual(3);
  });

  it('refreshes the list after the overlay reports a revert', async () => {
    session.meta.set(META);
    await settle();
    rows()[0].click();
    await settle();
    const before = engine.sessionChanges.calls.count();
    const overlay = fixture.debugElement.query(By.directive(DiffOverlay));
    expect(overlay).not.toBeNull();
    (overlay.componentInstance as DiffOverlay).reverted.emit('src/a.ts');
    await settle();
    expect(engine.sessionChanges.calls.count()).toBe(before + 1);
    (overlay.componentInstance as DiffOverlay).closed.emit();
    await settle();
    expect(fixture.componentInstance.openPath()).toBeNull();
  });
});
