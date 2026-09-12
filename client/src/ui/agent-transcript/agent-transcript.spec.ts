/**
 * F6-13: the read-only sub-agent transcript overlay loads the child session
 * once and then follows SSE updates for that session id only: a
 * `message.part.updated` snapshot patches (or appends) by index, events for
 * other sessions are ignored, and there is no composer anywhere.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { EngineClient } from '../../core/engine-client.service';
import { EngineEvent, Message } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { AgentTranscript } from './agent-transcript';

function message(id: string, role: 'user' | 'assistant', text: string): Message {
  return { id, role, parts: [{ type: 'text', text }] } as Message;
}

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

describe('AgentTranscript (F6-13)', () => {
  let fixture: ComponentFixture<AgentTranscript>;
  let engine: { messages: jasmine.Spy };
  let listener: ((event: EngineEvent) => void) | null;
  let closed: jasmine.Spy;

  beforeEach(() => {
    localStorage.clear();
    listener = null;
    engine = {
      messages: jasmine
        .createSpy('messages')
        .and.resolveTo([message('u1', 'user', 'please fix the CI')]),
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
      imports: [AgentTranscript],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: engine },
        { provide: EventsStore, useValue: events },
      ],
    });
    fixture = TestBed.createComponent(AgentTranscript);
    fixture.componentRef.setInput('sessionId', 'child-1');
    fixture.componentRef.setInput('name', 'fix-ci');
    fixture.componentRef.setInput('agent', 'code');
    fixture.componentRef.setInput('status', 'running');
    closed = jasmine.createSpy('closed');
    fixture.componentInstance.closed.subscribe(closed);
    fixture.detectChanges();
  });

  afterEach(() => fixture.destroy());

  async function settle(): Promise<void> {
    await wait(0);
    await fixture.whenStable();
    fixture.detectChanges();
  }

  function rowsText(): string[] {
    return Array.from(fixture.nativeElement.querySelectorAll('app-message-row')).map((el) =>
      (el as HTMLElement).textContent?.trim() ?? '',
    );
  }

  it('loads the child transcript once and renders it read-only', async () => {
    await settle();
    expect(engine.messages).toHaveBeenCalledWith('child-1');
    expect(engine.messages).toHaveBeenCalledTimes(1);
    expect(rowsText().length).toBe(1);
    expect(rowsText()[0]).toContain('please fix the CI');
    expect(fixture.nativeElement.querySelector('textarea')).toBeNull();
    expect(fixture.nativeElement.querySelector('.rollback')).toBeNull();
    expect(fixture.nativeElement.textContent).toContain('fix-ci');
    expect(fixture.nativeElement.textContent).toContain('running');
  });

  it('appends a streamed assistant message from a message.part.updated event', async () => {
    await settle();
    expect(listener).toBeTruthy();
    listener!({
      type: 'message.part.updated',
      directory: '/p',
      sessionID: 'child-1',
      properties: { messageIndex: 1, message: message('a1', 'assistant', 'On it') },
    });
    await settle();
    expect(rowsText().length).toBe(2);
    expect(rowsText()[1]).toContain('On it');

    // The same index again is a patch, not another append.
    listener!({
      type: 'message.part.updated',
      directory: '/p',
      sessionID: 'child-1',
      properties: { messageIndex: 1, message: message('a1', 'assistant', 'On it - done') },
    });
    await settle();
    expect(rowsText().length).toBe(2);
    expect(rowsText()[1]).toContain('On it - done');
    expect(engine.messages).toHaveBeenCalledTimes(1);
  });

  it('ignores events for other sessions', async () => {
    await settle();
    listener!({
      type: 'message.part.updated',
      directory: '/p',
      sessionID: 'someone-else',
      properties: { messageIndex: 1, message: message('x', 'assistant', 'not mine') },
    });
    await settle();
    expect(rowsText().length).toBe(1);
    expect(fixture.nativeElement.textContent).not.toContain('not mine');
  });

  it('refetches the transcript when the child turn settles', async () => {
    await settle();
    engine.messages.and.resolveTo([
      message('u1', 'user', 'please fix the CI'),
      message('a1', 'assistant', 'Fixed.'),
    ]);
    listener!({
      type: 'session.updated',
      directory: '/p',
      sessionID: 'child-1',
      properties: { running: false },
    });
    await wait(200);
    await settle();
    expect(engine.messages).toHaveBeenCalledTimes(2);
    expect(rowsText().length).toBe(2);
    expect(rowsText()[1]).toContain('Fixed.');
  });

  it('closes on the close button and on a backdrop click', async () => {
    await settle();
    (fixture.nativeElement.querySelector('button.close') as HTMLButtonElement).click();
    expect(closed).toHaveBeenCalledTimes(1);
    (fixture.nativeElement.querySelector('.backdrop') as HTMLElement).click();
    expect(closed).toHaveBeenCalledTimes(2);
    // A click inside the card must not close.
    (fixture.nativeElement.querySelector('.card') as HTMLElement).click();
    expect(closed).toHaveBeenCalledTimes(2);
  });
});
