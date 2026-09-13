/**
 * F9-12: the breadcrumb of a sub-agent chat reads "<parent> ↳ <child>" with
 * the parent clickable; an ordinary chat keeps the monospace session id.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { SessionMeta } from '../../core/engine.dtos';
import { ChatSessionStore } from '../../views/chat/chat-session.store';
import { ProjectSessionsStore } from '../shell/project-sessions.store';
import { ShellStore } from '../shell/shell.store';
import { Topbar } from './topbar';

function session(overrides: Partial<SessionMeta>): SessionMeta {
  const now = Date.now();
  return {
    id: 'session-0000',
    directory: '/p',
    agent: 'code',
    created_at: now,
    updated_at: now,
    usage: { input_tokens: 0, output_tokens: 0 },
    ...overrides,
  } as SessionMeta;
}

describe('Topbar (F9-12 sub-agent breadcrumb)', () => {
  let fixture: ComponentFixture<Topbar>;
  let currentSessionId: ReturnType<typeof signal<string | null>>;
  let sessions: ReturnType<typeof signal<SessionMeta[]>>;
  let chat: ChatSessionStore;

  beforeEach(() => {
    currentSessionId = signal<string | null>('child-1');
    sessions = signal<SessionMeta[]>([]);
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [Topbar],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        {
          provide: ShellStore,
          useValue: {
            isChat: signal(true),
            currentSessionId,
            activeScreen: signal('chat'),
            rightDrawerOpen: signal(true),
            toggleRightDrawer: () => undefined,
          },
        },
        { provide: ProjectSessionsStore, useValue: { sessions } },
      ],
    });
    chat = TestBed.inject(ChatSessionStore);
    fixture = TestBed.createComponent(Topbar);
    fixture.detectChanges();
  });

  it('shows the session id for an ordinary chat', () => {
    chat.meta.set(session({ id: 'child-1', title: 'Plain' }));
    fixture.detectChanges();
    const host = fixture.nativeElement as HTMLElement;
    expect(host.querySelector('[data-testid="crumb-parent"]')).toBeNull();
    expect(host.querySelector('.crumb-current.mono')?.textContent).toContain('child-1');
  });

  it('shows "<parent> ↳ <child>" with a link to the parent for a sub-agent chat', () => {
    sessions.set([session({ id: 'parent-1', title: 'Add the Orders feature' })]);
    chat.meta.set(session({ id: 'child-1', alias: 'api-orders', parent: ['parent-1', 3] }));
    fixture.detectChanges();
    const host = fixture.nativeElement as HTMLElement;
    const parent = host.querySelector('[data-testid="crumb-parent"]') as HTMLAnchorElement;
    expect(parent.textContent?.trim()).toBe('Add the Orders feature');
    expect(parent.getAttribute('href')).toBe('/chat/parent-1');
    expect(host.querySelector('[data-testid="crumb-child"]')?.textContent?.trim()).toBe(
      'api-orders',
    );
    expect(host.querySelector('.crumb-subagent svg')).toBeTruthy();
  });

  it('falls back to the short parent id while the session list has not loaded', () => {
    chat.meta.set(session({ id: 'child-1', alias: 'api-orders', parent: ['parent-1234-5678', 3] }));
    fixture.detectChanges();
    const parent = (fixture.nativeElement as HTMLElement).querySelector(
      '[data-testid="crumb-parent"]',
    );
    expect(parent?.textContent?.trim()).toBe('parent-1');
  });

  it('a fork (parent without alias) is not a sub-agent', () => {
    chat.meta.set(session({ id: 'child-1', title: 'Forked', parent: ['parent-1', 3] }));
    fixture.detectChanges();
    expect(
      (fixture.nativeElement as HTMLElement).querySelector('[data-testid="crumb-parent"]'),
    ).toBeNull();
  });
});
