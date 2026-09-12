/**
 * F6-13: the Agents panel renders `GET /session/{id}/agents` and refetches it
 * on `task.started` / `task.ended`, so a child moves running -> done without
 * polling. `EngineClient` and `EventsStore` are mocked; the SSE listener the
 * panel registers is captured and driven by hand.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { EngineClient } from '../../../core/engine-client.service';
import { AgentEntry, EngineEvent, SessionMeta } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { AgentsPanel } from './agents-panel';

const META: SessionMeta = {
  id: 'parent-1',
  directory: '/p',
  agent: 'orchestrator',
  created_at: 0,
  updated_at: 0,
  usage: { input_tokens: 0, output_tokens: 0 },
} as SessionMeta;

function entry(status: AgentEntry['status'], extra: Partial<AgentEntry> = {}): AgentEntry {
  return {
    taskID: status === 'running' ? 'task-1' : undefined,
    childSessionID: 'child-1',
    name: 'fix-ci',
    agent: 'code',
    model: 'test-model',
    status,
    description: 'please fix the CI',
    startedAt: Date.now() - 5_000,
    endedAt: status === 'running' ? undefined : Date.now(),
    usage: { input_tokens: 120, output_tokens: 30 },
    ...extra,
  };
}

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

describe('AgentsPanel (F6-13)', () => {
  let fixture: ComponentFixture<AgentsPanel>;
  let engine: { sessionAgents: jasmine.Spy; messages: jasmine.Spy };
  let listener: ((event: EngineEvent) => void) | null;
  let session: ChatSessionStore;

  beforeEach(() => {
    localStorage.clear();
    listener = null;
    engine = {
      sessionAgents: jasmine.createSpy('sessionAgents').and.resolveTo([entry('running')]),
      messages: jasmine.createSpy('messages').and.resolveTo([]),
    };
    const events = {
      reconnectVersion: signal(0),
      onEvent: jasmine.createSpy('onEvent').and.callFake((fn: (event: EngineEvent) => void) => {
        listener = fn;
        return () => {
          listener = null;
        };
      }),
    };
    TestBed.configureTestingModule({
      imports: [AgentsPanel],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: engine },
        { provide: EventsStore, useValue: events },
      ],
    });
    session = TestBed.inject(ChatSessionStore);
    session.meta.set(META);
    fixture = TestBed.createComponent(AgentsPanel);
    fixture.detectChanges();
  });

  afterEach(() => fixture.destroy());

  async function settle(): Promise<void> {
    await wait(0);
    await fixture.whenStable();
    fixture.detectChanges();
  }

  function rows(): HTMLElement[] {
    return Array.from(fixture.nativeElement.querySelectorAll('button.agent'));
  }

  it('fetches the agents of the open session and renders a running row', async () => {
    await settle();
    expect(engine.sessionAgents).toHaveBeenCalledWith('parent-1');
    const [row] = rows();
    expect(row).toBeTruthy();
    expect(row.getAttribute('data-status')).toBe('running');
    expect(row.textContent).toContain('fix-ci');
    expect(row.textContent).toContain('code');
    expect(row.textContent).toContain('test-model');
    expect(row.textContent).toContain('running');
  });

  it('moves the row to done after a task.ended event (refetch, no polling)', async () => {
    await settle();
    expect(rows()[0].getAttribute('data-status')).toBe('running');
    expect(listener).toBeTruthy();

    engine.sessionAgents.and.resolveTo([entry('done')]);
    listener!({
      type: 'task.ended',
      directory: '/p',
      sessionID: 'parent-1',
      properties: { taskID: 'task-1', status: 'completed', childSessionID: 'child-1' },
    });
    // The refetch is debounced (bursts from `fleet`), so give it a moment.
    await wait(250);
    await settle();

    expect(engine.sessionAgents).toHaveBeenCalledTimes(2);
    const [row] = rows();
    expect(row.getAttribute('data-status')).toBe('done');
    expect(row.textContent).toContain('done');
    expect(row.textContent).toContain('tokens');
  });

  it('ignores task events for other sessions', async () => {
    await settle();
    listener!({
      type: 'task.ended',
      directory: '/p',
      sessionID: 'someone-else',
      properties: { taskID: 'x' },
    });
    await wait(250);
    await settle();
    expect(engine.sessionAgents).toHaveBeenCalledTimes(1);
  });

  it('renders the failed status with its error text', async () => {
    engine.sessionAgents.and.resolveTo([entry('failed', { error: 'provider exploded' })]);
    listener!({ type: 'task.ended', directory: '/p', sessionID: 'parent-1', properties: {} });
    await wait(250);
    await settle();
    const [row] = rows();
    expect(row.getAttribute('data-status')).toBe('failed');
    expect(row.textContent).toContain('provider exploded');
  });

  it('opens the read-only transcript overlay for a clicked row', async () => {
    await settle();
    rows()[0].click();
    await settle();
    const overlay = fixture.nativeElement.querySelector('[data-testid="agent-transcript"]');
    expect(overlay).toBeTruthy();
    expect(engine.messages).toHaveBeenCalledWith('child-1');
    expect(fixture.nativeElement.querySelector('textarea')).toBeNull();
  });

  it('shows the empty state when nothing was delegated', async () => {
    engine.sessionAgents.and.resolveTo([]);
    session.meta.set({ ...META, id: 'parent-2' });
    await settle();
    await settle();
    expect(rows().length).toBe(0);
    expect(fixture.nativeElement.textContent).toContain('No sub-agents');
  });
});
