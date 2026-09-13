/**
 * WP-M6 (F10-25): the permission prompt in `sheet` mode - renders as a bottom
 * sheet on `permission.asked`, posts the decision, dismisses on
 * `permission.resolved`, and queues the decision when the desktop is
 * unreachable (F10-27).
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { ENGINE_API } from '../../core/engine-api';
import { EngineEvent } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { OfflineQueue } from '../../core/remote/offline-cache';
import { PermissionPopup } from './permission-popup';

describe('PermissionPopup sheet mode (WP-M6 / F10-25)', () => {
  let fixture: ComponentFixture<PermissionPopup>;
  let engine: { resolvePermission: jasmine.Spy; pendingPermissions: jasmine.Spy; connection: ReturnType<typeof signal<null>>; unauthorized: ReturnType<typeof signal<boolean>> };
  let listener: ((ev: EngineEvent) => void) | null;

  const asked: EngineEvent = {
    type: 'permission.asked',
    directory: 'C:/p',
    sessionID: 's1',
    properties: {
      requestID: 'r1',
      messageIndex: 3,
      toolName: 'write_file',
      agent: 'code',
      pattern: 'write_file(mock-1.txt)',
      suggestedRule: 'write_file(*)',
      input: { path: 'mock-1.txt', content: 'hi' },
    },
  };

  beforeEach(() => {
    localStorage.clear();
    listener = null;
    engine = {
      connection: signal(null),
      unauthorized: signal(false),
      resolvePermission: jasmine.createSpy('resolvePermission').and.resolveTo({ resolved: true }),
      pendingPermissions: jasmine.createSpy('pendingPermissions').and.resolveTo([]),
    };
    const events = {
      state: signal('live'),
      onEvent: jasmine.createSpy('onEvent').and.callFake((fn: (ev: EngineEvent) => void) => {
        listener = fn;
        return () => (listener = null);
      }),
    };
    TestBed.configureTestingModule({
      imports: [PermissionPopup],
      providers: [
        provideZonelessChangeDetection(),
        { provide: EngineClient, useValue: engine },
        { provide: ENGINE_API, useValue: engine },
        { provide: EventsStore, useValue: events },
      ],
    });
    fixture = TestBed.createComponent(PermissionPopup);
    fixture.componentRef.setInput('mode', 'sheet');
    fixture.componentRef.setInput('activeSessionID', 's1');
    fixture.componentRef.setInput('directory', 'C:/p');
    fixture.detectChanges();
  });

  afterEach(() => {
    fixture.destroy();
    localStorage.clear();
  });

  async function settle(): Promise<void> {
    for (let i = 0; i < 4; i++) {
      await Promise.resolve();
    }
    fixture.detectChanges();
  }

  const el = (): HTMLElement => fixture.nativeElement as HTMLElement;

  it('renders nothing until an ask arrives, then a fixed bottom sheet with backdrop', async () => {
    expect(el().querySelector('.perm')).toBeNull();
    listener!(asked);
    await settle();
    const dialog = el().querySelector<HTMLElement>('.perm');
    expect(dialog).not.toBeNull();
    expect(dialog!.classList).toContain('perm-sheet');
    expect(dialog!.getAttribute('data-mode')).toBe('sheet');
    expect(el().querySelector('[data-testid="perm-sheet-backdrop"]')).not.toBeNull();
    expect(el().querySelector('.sheet-handle')).not.toBeNull();
    expect(el().querySelector('[data-testid="perm-pattern"]')?.textContent).toContain('write_file(*)');
    expect(dialog!.textContent).toContain('write_file');
    // The sheet is a real dialog for assistive tech.
    expect(dialog!.getAttribute('role')).toBe('dialog');
    expect(getComputedStyle(dialog!).position).toBe('fixed');
  });

  it('shows the sub-agent name for a child-session ask', async () => {
    listener!({
      ...asked,
      sessionID: 'child-1',
      properties: { ...asked.properties, sessionAlias: 'api-orders', parentSessionID: 's1' },
    });
    await settle();
    expect(el().querySelector('[data-testid="perm-body"]')?.textContent).toContain('api-orders');
  });

  it('Allow posts the decision against the asking session and closes the sheet', async () => {
    listener!(asked);
    await settle();
    el().querySelector<HTMLButtonElement>('.allow')!.click();
    await settle();
    expect(engine.resolvePermission).toHaveBeenCalledWith('s1', 'r1', { decision: 'allow', always: false });
    expect(el().querySelector('.perm')).toBeNull();
  });

  it('Always allow posts always:true; Deny posts deny', async () => {
    listener!(asked);
    await settle();
    el().querySelector<HTMLButtonElement>('[data-testid="perm-always"]')!.click();
    await settle();
    expect(engine.resolvePermission).toHaveBeenCalledWith('s1', 'r1', { decision: 'allow', always: true });
    listener!({ ...asked, properties: { ...asked.properties, requestID: 'r2' } });
    await settle();
    el().querySelector<HTMLButtonElement>('.deny')!.click();
    await settle();
    expect(engine.resolvePermission).toHaveBeenCalledWith('s1', 'r2', { decision: 'deny', always: false });
  });

  it('dismisses on permission.resolved from elsewhere (desktop answered first)', async () => {
    listener!(asked);
    await settle();
    expect(el().querySelector('.perm')).not.toBeNull();
    listener!({ type: 'permission.resolved', directory: 'C:/p', sessionID: 's1', properties: { requestID: 'r1' } });
    await settle();
    expect(el().querySelector('.perm')).toBeNull();
    expect(engine.resolvePermission).not.toHaveBeenCalled();
  });

  it('queues the decision when the desktop is unreachable (F10-27)', async () => {
    engine.resolvePermission.and.rejectWith(new TypeError('Failed to fetch'));
    const queue = TestBed.inject(OfflineQueue);
    listener!(asked);
    await settle();
    el().querySelector<HTMLButtonElement>('.allow')!.click();
    await settle();
    expect(queue.pending().length).toBe(1);
    expect(queue.pending()[0]).toEqual(
      jasmine.objectContaining({
        kind: 'permission',
        sessionID: 's1',
        payload: { requestID: 'r1', decision: 'allow', always: false },
      }),
    );
  });
});
