/**
 * F6-13: the Agents panel renders `GET /session/{id}/agents` and refetches it
 * on `task.started` / `task.ended`, so a child moves running -> done without
 * polling. `EngineClient` and `EventsStore` are mocked; the SSE listener the
 * panel registers is captured and driven by hand.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { Router, provideRouter } from '@angular/router';

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

  // -- F9-4 ------------------------------------------------------------------

  it('F9-4: the whole card is the transcript trigger (keyboard: Enter on the focused card)', async () => {
    await settle();
    const [card] = rows();
    expect(card.getAttribute('data-testid')).toBe('agent-card');
    expect(card.getAttribute('aria-label')).toContain('fix-ci');
    card.focus();
    card.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    card.click(); // a native <button> maps Enter/Space to click
    await settle();
    expect(fixture.nativeElement.querySelector('[data-testid="agent-transcript"]')).toBeTruthy();
    expect(fixture.nativeElement.querySelector('.view-hint')?.textContent).toContain(
      'View transcript',
    );
  });

  it('F9-4: "Open session" navigates to the child chat and never opens the overlay', async () => {
    await settle();
    const router = TestBed.inject(Router);
    const navigate = spyOn(router, 'navigate').and.resolveTo(true);
    const open = fixture.nativeElement.querySelector(
      '[data-testid="agent-open-session"]',
    ) as HTMLButtonElement;
    expect(open).toBeTruthy();
    expect(open.textContent).toContain('Open session');
    open.click();
    await settle();
    expect(navigate).toHaveBeenCalledWith(['/chat', 'child-1']);
    expect(fixture.nativeElement.querySelector('[data-testid="agent-transcript"]')).toBeNull();
  });

  it('shows the empty state when nothing was delegated', async () => {
    engine.sessionAgents.and.resolveTo([]);
    session.meta.set({ ...META, id: 'parent-2' });
    await settle();
    await settle();
    expect(rows().length).toBe(0);
    expect(fixture.nativeElement.textContent).toContain('No sub-agents');
  });

  // -- WP-DELEGATION (F8-2) --------------------------------------------------

  it('patches a running row in place from task.progress without a refetch', async () => {
    await settle();
    expect(engine.sessionAgents).toHaveBeenCalledTimes(1);
    listener!({
      type: 'task.progress',
      directory: '/p',
      sessionID: 'parent-1',
      properties: {
        taskID: 'task-1',
        childSessionID: 'child-1',
        name: 'fix-ci',
        agent: 'code',
        status: 'running',
        progress: {
          lastTool: 'write_file',
          lastToolState: 'running',
          summary: 'Adding the About route',
          toolCalls: 4,
          steps: 2,
        },
        tokens: { input: 4000, output: 500 },
        at: Date.now(),
      },
    });
    await wait(250);
    await settle();
    expect(engine.sessionAgents).toHaveBeenCalledTimes(1);
    const line = fixture.nativeElement.querySelector('[data-testid="task-progress-line"]');
    expect(line).toBeTruthy();
    expect(line.textContent).toContain('write_file');
    expect(line.textContent).toContain('Adding the About route');
    expect(line.textContent).toContain('4 calls');
    expect(line.textContent).toContain('4.5k');
  });

  it('renders a queued child (waiting for a concurrency slot) as queued', async () => {
    engine.sessionAgents.and.resolveTo([entry('queued', { taskID: 'task-q' })]);
    session.meta.set({ ...META, id: 'parent-3' });
    await settle();
    await settle();
    const [row] = rows();
    expect(row.getAttribute('data-status')).toBe('queued');
    expect(row.textContent).toContain('queued');
    expect(row.textContent).toContain('waiting for a free slot');
  });
});
