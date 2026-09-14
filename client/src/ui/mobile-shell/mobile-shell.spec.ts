/**
 * WP-M2 / F10-8: the mobile shell renders the five bottom tabs, the active
 * tab (and the title) follow the router, and the engine sheet opens/closes.
 */

import { Component, provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { EngineTargetStore } from '../../core/engine-target.store';
import { EventsStore, SseState } from '../../core/events.store';
import { ProjectsStore } from '../../core/projects.store';
import { MobileShell } from './mobile-shell';
import { DISMISS_PX, Sheet } from './sheet';

@Component({ selector: 'test-blank', template: '' })
class Blank {}

const testRoutes = [
  {
    path: 'm',
    children: [
      { path: 'chat', component: Blank, data: { tab: 'chat' } },
      { path: 'chat/:sessionID', component: Blank, data: { tab: 'chat' } },
      { path: 'remote', component: Blank, data: { tab: 'remote' } },
      { path: 'agents', component: Blank, data: { tab: 'agents' } },
      { path: 'changes', component: Blank, data: { tab: 'changes' } },
      { path: 'more', component: Blank, data: { tab: 'more' } },
      { path: 'more/stats', component: Blank, data: { tab: 'more' } },
    ],
  },
];

describe('MobileShell', () => {
  let fixture: ComponentFixture<MobileShell>;
  let router: Router;
  let targets: EngineTargetStore;
  let engine: {
    connect: jasmine.Spy;
    switchTarget: jasmine.Spy;
    unauthorized: ReturnType<typeof signal<boolean>>;
    viaRelay: ReturnType<typeof signal<boolean>>;
    isTauri: ReturnType<typeof signal<boolean>>;
    isCapacitor: boolean;
    connection: ReturnType<typeof signal<null>>;
    remoteDefaults: () => { baseUrl: string };
  };
  let sseState: ReturnType<typeof signal<SseState>>;

  function el<T extends HTMLElement>(testId: string): T | null {
    return (fixture.nativeElement as HTMLElement).querySelector<T>(`[data-testid="${testId}"]`);
  }

  async function settle(): Promise<void> {
    await fixture.whenStable();
    fixture.detectChanges();
  }

  beforeEach(async () => {
    localStorage.clear();
    sseState = signal<SseState>('idle');
    engine = {
      connect: jasmine.createSpy('connect').and.callFake(async () => {
        sseState.set('live');
        return { kind: 'http', baseUrl: 'http://a:1' };
      }),
      switchTarget: jasmine.createSpy('switchTarget').and.resolveTo(undefined),
      unauthorized: signal(false),
      viaRelay: signal(false),
      isTauri: signal(false),
      isCapacitor: false,
      connection: signal(null),
      remoteDefaults: () => ({ baseUrl: 'http://a:1' }),
    };
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        provideRouter(testRoutes),
        { provide: EngineClient, useValue: engine },
        {
          provide: EventsStore,
          useValue: { state: sseState, desktopOffline: signal(false), start: jasmine.createSpy('start') },
        },
        {
          provide: ProjectsStore,
          useValue: { refresh: jasmine.createSpy('refresh').and.resolveTo(undefined) },
        },
      ],
    });
    router = TestBed.inject(Router);
    targets = TestBed.inject(EngineTargetStore);
    fixture = TestBed.createComponent(MobileShell);
    await router.navigateByUrl('/m/chat');
    await settle();
  });

  afterEach(() => localStorage.clear());

  it('boots the connection and renders the five tabs in order', () => {
    expect(engine.connect).toHaveBeenCalled();
    const tabs = Array.from(
      (fixture.nativeElement as HTMLElement).querySelectorAll<HTMLElement>('[data-tab]'),
    ).map((a) => a.dataset['tab']);
    expect(tabs).toEqual(['chat', 'remote', 'agents', 'changes', 'more']);
    expect(el('mobile-status')?.textContent).toContain('engine live');
  });

  it('the active tab and the title follow the router', async () => {
    expect(el('tab-chat')?.classList).toContain('active');
    expect(el('mobile-title')?.textContent?.trim()).toBe('Chat');

    await router.navigateByUrl('/m/agents');
    await settle();
    expect(el('tab-agents')?.classList).toContain('active');
    expect(el('tab-chat')?.classList).not.toContain('active');
    expect(el('mobile-title')?.textContent?.trim()).toBe('Agents');

    // Nested screens keep their tab lit (prefix match).
    await router.navigateByUrl('/m/more/stats');
    await settle();
    expect(el('tab-more')?.classList).toContain('active');
    expect(el('mobile-title')?.textContent?.trim()).toBe('More');

    await router.navigateByUrl('/m/chat/s1');
    await settle();
    expect(el('tab-chat')?.classList).toContain('active');
  });

  it('opens the engine sheet from the overflow button and closes it from the backdrop', async () => {
    expect(el('sheet')).toBeNull();
    el<HTMLButtonElement>('mobile-overflow')!.click();
    await settle();
    expect(el('sheet')).not.toBeNull();
    expect(el('sheet')?.getAttribute('aria-label')).toBe('Engine');

    el('sheet-backdrop')!.click();
    await settle();
    expect(el('sheet')).toBeNull();
  });

  it('lists the known targets and switches on tap', async () => {
    targets.upsert({ id: 'a', kind: 'embedded', label: 'This device', baseUrl: 'http://a:1', token: null });
    targets.upsert({ id: 'd', kind: 'desktop', label: 'Desktop', baseUrl: 'http://d:2', token: 't' });
    targets.setActive('a');
    el<HTMLButtonElement>('mobile-status')!.click();
    await settle();

    expect(el('target-a')?.classList).toContain('active');
    expect(el('target-a')?.textContent).toContain('active');
    expect(el('target-d')?.textContent).toContain('desktop');

    el<HTMLButtonElement>('target-d')!.click();
    await settle();
    expect(engine.switchTarget).toHaveBeenCalledWith('d');
    expect(el('sheet')).toBeNull();
  });
});

describe('Sheet', () => {
  @Component({
    imports: [Sheet],
    template: `<app-sheet [open]="open()" title="T" (close)="open.set(false)">body</app-sheet>`,
  })
  class Host {
    readonly open = signal(true);
  }

  let fixture: ComponentFixture<Host>;

  function q(testId: string): HTMLElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector(`[data-testid="${testId}"]`);
  }

  beforeEach(async () => {
    TestBed.configureTestingModule({ providers: [provideZonelessChangeDetection()] });
    fixture = TestBed.createComponent(Host);
    await fixture.whenStable();
  });

  it('renders content and title while open, nothing when closed', async () => {
    expect(q('sheet')?.textContent).toContain('body');
    expect(q('sheet')?.textContent).toContain('T');
    fixture.componentInstance.open.set(false);
    await fixture.whenStable();
    expect(q('sheet')).toBeNull();
  });

  it('closes on Escape', async () => {
    document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape' }));
    await fixture.whenStable();
    expect(fixture.componentInstance.open()).toBeFalse();
  });

  it('drag-to-dismiss: a short drag snaps back, a long one closes', async () => {
    const handle = q('sheet-handle')!;
    handle.dispatchEvent(new PointerEvent('pointerdown', { clientY: 100, pointerId: 1 }));
    handle.dispatchEvent(new PointerEvent('pointermove', { clientY: 130, pointerId: 1 }));
    await fixture.whenStable();
    expect(q('sheet')?.style.transform).toBe('translateY(30px)');
    handle.dispatchEvent(new PointerEvent('pointerup', { clientY: 130, pointerId: 1 }));
    await fixture.whenStable();
    expect(fixture.componentInstance.open()).toBeTrue();
    expect(q('sheet')?.style.transform).toBe('');

    handle.dispatchEvent(new PointerEvent('pointerdown', { clientY: 100, pointerId: 1 }));
    handle.dispatchEvent(
      new PointerEvent('pointermove', { clientY: 100 + DISMISS_PX + 1, pointerId: 1 }),
    );
    handle.dispatchEvent(
      new PointerEvent('pointerup', { clientY: 100 + DISMISS_PX + 1, pointerId: 1 }),
    );
    await fixture.whenStable();
    expect(fixture.componentInstance.open()).toBeFalse();
  });
});
