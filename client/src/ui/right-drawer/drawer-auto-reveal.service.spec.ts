/**
 * F9-2: agent-driven reveals - Agents on `task.started`, Browser on the first
 * `browser_*` tool part - for the session open in chat only, at most once per
 * id, and never re-opening a drawer the user closed.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { TestBed } from '@angular/core/testing';

import { EngineEvent } from '../../core/engine.dtos';
import { EventsStore } from '../../core/events.store';
import { UiPrefsStore } from '../../core/ui-prefs.store';
import { ShellStore } from '../shell/shell.store';
import { DrawerAutoReveal } from './drawer-auto-reveal.service';

function partEvent(sessionID: string, toolName: string, id: string): EngineEvent {
  return {
    type: 'message.part.updated',
    directory: '/p',
    sessionID,
    properties: {
      messageIndex: 0,
      message: {
        id: 'm1',
        role: 'assistant',
        parts: [{ type: 'tool', id, name: toolName, state: { state: 'running', input: {} } }],
      },
    },
  };
}

describe('DrawerAutoReveal (F9-2)', () => {
  let service: DrawerAutoReveal;
  let prefs: UiPrefsStore;

  beforeEach(() => {
    localStorage.clear();
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      providers: [
        provideZonelessChangeDetection(),
        { provide: EventsStore, useValue: { onEvent: () => () => undefined } },
        {
          provide: ShellStore,
          useValue: { isChat: signal(true), currentSessionId: signal('s-1') },
        },
      ],
    });
    service = TestBed.inject(DrawerAutoReveal);
    prefs = TestBed.inject(UiPrefsStore);
    prefs.setRightDrawerPanel('agents', false);
    prefs.setRightDrawerPanel('browser', false);
  });

  it('reveals Agents when a sub-agent spawns in the open session (once per task)', () => {
    service.handle({
      type: 'task.started',
      directory: '/p',
      sessionID: 's-1',
      properties: { taskID: 't1' },
    });
    expect(prefs.rightDrawerPanels().agents).toBeTrue();
    expect(prefs.rightDrawerReveal()?.panel).toBe('agents');
    const nonce = prefs.rightDrawerReveal()?.nonce;
    service.handle({
      type: 'task.started',
      directory: '/p',
      sessionID: 's-1',
      properties: { taskID: 't1' },
    });
    expect(prefs.rightDrawerReveal()?.nonce).toBe(nonce);
  });

  it('ignores events of other sessions', () => {
    service.handle({
      type: 'task.started',
      directory: '/p',
      sessionID: 's-2',
      properties: { taskID: 't9' },
    });
    expect(prefs.rightDrawerPanels().agents).toBeFalse();
    expect(prefs.rightDrawerReveal()).toBeNull();
  });

  it('reveals Browser on the first browser_* tool call, not on other tools', () => {
    service.handle(partEvent('s-1', 'read_file', 'c0'));
    expect(prefs.rightDrawerPanels().browser).toBeFalse();
    service.handle(partEvent('s-1', 'browser_open', 'c1'));
    expect(prefs.rightDrawerPanels().browser).toBeTrue();
    expect(prefs.rightDrawerReveal()?.panel).toBe('browser');
    const nonce = prefs.rightDrawerReveal()?.nonce;
    // The same part is re-published as it progresses - no second reveal.
    service.handle(partEvent('s-1', 'browser_open', 'c1'));
    expect(prefs.rightDrawerReveal()?.nonce).toBe(nonce);
  });

  it('never re-opens a drawer the user closed', () => {
    prefs.setRightDrawerOpen(false);
    service.handle({
      type: 'task.started',
      directory: '/p',
      sessionID: 's-1',
      properties: { taskID: 't2' },
    });
    expect(prefs.rightDrawerOpen()).toBeFalse();
    expect(prefs.rightDrawerPanels().agents).toBeTrue();
  });
});
