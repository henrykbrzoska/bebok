/**
 * WP-M6 (F10-26): the read-only tabs render at phone width without any
 * control the remote scope would 403 - no revert / explorer in the diff
 * overlay, no kill in the process list - plus their empty states.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { EngineClient } from '../../../core/engine-client.service';
import { ENGINE_API } from '../../../core/engine-api';
import { EngineTargetStore } from '../../../core/engine-target.store';
import { EngineEvent, ProcessInfo, SessionMeta } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { MemoryCacheStore, OFFLINE_CACHE_STORE } from '../../../core/remote/offline-cache';
import { MobileSessionContext } from '../../../core/remote/session-context';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { AgentsTab } from './agents-tab';
import { ChangesTab } from './changes-tab';

const META: SessionMeta = {
  id: 's1',
  directory: 'C:/p',
  agent: 'code',
  title: 'Fix login',
  running: false,
  created_at: 0,
  updated_at: 0,
  usage: { input_tokens: 0, output_tokens: 0 },
};

const PROC: ProcessInfo = {
  id: 'proc-1',
  session_id: 's1',
  command: 'npm run dev',
  cwd: 'C:/p',
  pid: 4242,
  started_at: Date.now() - 65_000,
  status: 'running',
  log_path: 'C:/p/.bebok/run/proc-1.log',
  agent: 'code',
  url: 'http://localhost:4300',
};

describe('WP-M6 read-only tabs (F10-26)', () => {
  let engine: Record<string, jasmine.Spy | ReturnType<typeof signal<unknown>>> & {
    sessionMeta: jasmine.Spy;
    sessionChangeDiff: jasmine.Spy;
    revertSessionChange: jasmine.Spy;
    sessionAgents: jasmine.Spy;
    sessionProcesses: jasmine.Spy;
    processLog: jasmine.Spy;
    killProcess: jasmine.Spy;
  };
  let listener: ((ev: EngineEvent) => void)[];
  let chat: ChatSessionStore;

  beforeEach(() => {
    localStorage.clear();
    listener = [];
    engine = {
      connected: signal(true),
      connection: signal({ kind: 'http', baseUrl: 'http://1' }),
      unauthorized: signal(false),
      sessionMeta: jasmine.createSpy('sessionMeta').and.resolveTo(META),
      sessionChanges: jasmine.createSpy('sessionChanges').and.resolveTo([
        { path: 'src/a.ts', added: 3, removed: 1, baseline: 'git', exists: true },
      ]),
      sessionChangeDiff: jasmine.createSpy('sessionChangeDiff').and.resolveTo({
        path: 'src/a.ts',
        diff: '--- a/src/a.ts\n+++ b/src/a.ts\n@@ -1,1 +1,1 @@\n-old\n+new',
        baseline: 'git',
        added: 1,
        removed: 1,
      }),
      revertSessionChange: jasmine.createSpy('revertSessionChange'),
      sessionAgents: jasmine.createSpy('sessionAgents').and.resolveTo([]),
      sessionProcesses: jasmine.createSpy('sessionProcesses').and.resolveTo([PROC]),
      processLog: jasmine.createSpy('processLog').and.resolveTo({ id: 'proc-1', log: 'ready on 4300\n', size: 14 }),
      killProcess: jasmine.createSpy('killProcess'),
      listProjects: jasmine.createSpy('listProjects').and.resolveTo([]),
      listSessions: jasmine.createSpy('listSessions').and.resolveTo([]),
    };
    const events = {
      state: signal('live'),
      reconnectVersion: signal(0),
      onEvent: jasmine.createSpy('onEvent').and.callFake((fn: (ev: EngineEvent) => void) => {
        listener.push(fn);
        return () => undefined;
      }),
    };
    TestBed.configureTestingModule({
      imports: [ChangesTab, AgentsTab],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: engine },
        { provide: ENGINE_API, useValue: engine },
        { provide: EventsStore, useValue: events },
        { provide: OFFLINE_CACHE_STORE, useValue: new MemoryCacheStore() },
      ],
    });
    chat = TestBed.inject(ChatSessionStore);
    const targets = TestBed.inject(EngineTargetStore);
    targets.upsert({ id: 'desktop:1', kind: 'desktop', label: 'd', baseUrl: 'http://1', token: 't' });
    targets.setActive('desktop:1');
  });

  afterEach(() => {
    chat.clear();
    localStorage.clear();
  });

  async function settle(fixture: ComponentFixture<unknown>): Promise<void> {
    for (let i = 0; i < 8; i++) {
      await Promise.resolve();
    }
    fixture.detectChanges();
  }

  function q(fixture: ComponentFixture<unknown>, selector: string): HTMLElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector(selector);
  }

  describe('Changes tab', () => {
    it('empty state without a remembered session', async () => {
      const fixture = TestBed.createComponent(ChangesTab);
      fixture.detectChanges();
      await settle(fixture);
      expect(q(fixture, '[data-testid="session-host-empty"]')).not.toBeNull();
      expect(engine.sessionMeta).not.toHaveBeenCalled();
      fixture.destroy();
    });

    it('publishes the remembered session and lists its changes; the overlay has no revert / explorer', async () => {
      TestBed.inject(MobileSessionContext).remember('s1', 'Fix login');
      const fixture = TestBed.createComponent(ChangesTab);
      fixture.detectChanges();
      await settle(fixture);
      expect(engine.sessionMeta).toHaveBeenCalledWith('s1');
      expect(chat.meta()?.id).toBe('s1');
      expect(q(fixture, '[data-testid="session-host-head"]')?.textContent).toContain('Fix login');
      await settle(fixture);
      const row = q(fixture, '[data-testid="change-row"]');
      expect(row).not.toBeNull();
      row!.click();
      await settle(fixture);
      expect(engine.sessionChangeDiff).toHaveBeenCalled();
      expect(q(fixture, 'app-diff-overlay')).not.toBeNull();
      expect(q(fixture, '[data-testid="revert-file"]')).toBeNull();
      expect(q(fixture, '[data-testid="open-explorer"]')).toBeNull();
      expect(q(fixture, '.card')?.classList).toContain('mobile');
      // Unified only: the split toggle is gone.
      expect(q(fixture, '.modes')).toBeNull();
      expect(engine.revertSessionChange).not.toHaveBeenCalled();
      fixture.destroy();
      // Leaving the tab releases the published session.
      expect(chat.meta()).toBeNull();
    });
  });

  describe('Agents tab', () => {
    it('shows the agents segment by default and the empty agents state', async () => {
      TestBed.inject(MobileSessionContext).remember('s1');
      const fixture = TestBed.createComponent(AgentsTab);
      fixture.detectChanges();
      await settle(fixture);
      await settle(fixture);
      expect(q(fixture, 'app-agents-panel')).not.toBeNull();
      expect(engine.sessionAgents).toHaveBeenCalledWith('s1');
      fixture.destroy();
    });

    it('Processes: list + read-only log with live output, and no kill control', async () => {
      TestBed.inject(MobileSessionContext).remember('s1');
      const fixture = TestBed.createComponent(AgentsTab);
      fixture.detectChanges();
      await settle(fixture);
      q(fixture, '[data-testid="agents-segment-processes"]')!.click();
      await settle(fixture);
      await settle(fixture);
      expect(engine.sessionProcesses).toHaveBeenCalledWith('s1');
      const row = q(fixture, '[data-testid="process-proc-1"]');
      expect(row).not.toBeNull();
      expect(row!.textContent).toContain('npm run dev');
      expect(row!.textContent).toContain('http://localhost:4300');
      expect((fixture.nativeElement as HTMLElement).textContent).not.toMatch(/kill/i);
      row!.click();
      await settle(fixture);
      expect(engine.processLog).toHaveBeenCalledWith('proc-1', jasmine.any(Number));
      expect(q(fixture, '[data-testid="process-log-text"]')?.textContent).toContain('ready on 4300');
      // A coalesced process.output chunk lands in the log.
      for (const fn of listener) {
        fn({
          type: 'process.output',
          directory: 'C:/p',
          sessionID: 's1',
          properties: { id: 'proc-1', sessionID: 's1', chunk: 'compiled ok\n', at: Date.now(), coalesced: true },
        });
      }
      await settle(fixture);
      expect(q(fixture, '[data-testid="process-log-text"]')?.textContent).toContain('compiled ok');
      expect(engine.killProcess).not.toHaveBeenCalled();
      expect(q(fixture, 'button[title*="ill"]')).toBeNull();
      fixture.destroy();
    });
  });
});
