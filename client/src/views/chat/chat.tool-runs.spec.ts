/**
 * F6-1c: merge a run of >= 2 consecutive tool-only assistant turns (no text
 * part) into one `ToolRunRow`, rendered as a single `<app-tool-run-row>`
 * instead of one `<app-message-row>` per turn. `groupMessageRuns` operates
 * on whatever message list it is given, so `ChatView.transcriptRows` runs it
 * over the already-windowed slice (`windowedMessages()`, F6-2): a run can
 * span - and be split at - the "Load earlier" window edge, which is expected
 * (see `WP-CHAT2.md`'s scope note), not a bug.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { Message, Part } from '../../core/engine.dtos';
import { ChatView, MESSAGE_WINDOW, groupMessageRuns, isToolOnlyTurn } from './chat';

function text(id: string, value = 'hi'): Message {
  return { id, role: id.startsWith('u') ? 'user' : 'assistant', parts: [{ type: 'text', text: value }] };
}

function toolPart(name = 'read'): Part {
  return { type: 'tool', id: `${name}-${Math.random()}`, name, state: { state: 'completed', input: {}, output: 'ok', title: name } };
}

function toolOnly(id: string, extra: Part[] = []): Message {
  return { id, role: 'assistant', parts: [toolPart(), ...extra] };
}

describe('isToolOnlyTurn (F6-1c)', () => {
  it('is true for an assistant message with only tool/thinking/usage parts', () => {
    expect(isToolOnlyTurn(toolOnly('a'))).toBeTrue();
    expect(
      isToolOnlyTurn({
        id: 'a',
        role: 'assistant',
        parts: [{ type: 'thinking', text: 'hmm' }, toolPart(), { type: 'usage', input_tokens: 1, output_tokens: 1 }],
      }),
    ).toBeTrue();
  });

  it('is false once a text part is present, however placed', () => {
    expect(isToolOnlyTurn({ id: 'a', role: 'assistant', parts: [toolPart(), { type: 'text', text: 'done' }] })).toBeFalse();
    expect(isToolOnlyTurn({ id: 'a', role: 'assistant', parts: [{ type: 'text', text: 'done' }, toolPart()] })).toBeFalse();
  });

  it('is always false for a user message, even one carrying only non-text parts', () => {
    expect(isToolOnlyTurn({ id: 'u1', role: 'user', parts: [toolPart()] })).toBeFalse();
  });
});

describe('groupMessageRuns (F6-1c)', () => {
  it('leaves a transcript with no tool-only turns unchanged, one row per message', () => {
    const rows = groupMessageRuns([text('u1'), text('a1')]);
    expect(rows).toEqual([
      { kind: 'single', message: jasmine.objectContaining({ id: 'u1' }) },
      { kind: 'single', message: jasmine.objectContaining({ id: 'a1' }) },
    ]);
  });

  it('does not merge a single tool-only turn with no tool-only neighbour', () => {
    const rows = groupMessageRuns([text('u1'), toolOnly('a1'), text('u2')]);
    expect(rows.map((r) => r.kind)).toEqual(['single', 'single', 'single']);
  });

  it('merges a run of >= 2 consecutive tool-only turns into one run row', () => {
    const rows = groupMessageRuns([text('u1'), toolOnly('a1'), toolOnly('a2'), toolOnly('a3'), text('a4', 'done')]);
    expect(rows.length).toBe(3);
    expect(rows[0]).toEqual({ kind: 'single', message: jasmine.objectContaining({ id: 'u1' }) });
    expect(rows[1].kind).toBe('run');
    if (rows[1].kind === 'run') {
      expect(rows[1].key).toBe('a1');
      expect(rows[1].messages.map((m) => m.id)).toEqual(['a1', 'a2', 'a3']);
    }
    expect(rows[2]).toEqual({ kind: 'single', message: jasmine.objectContaining({ id: 'a4' }) });
  });

  it('a text-bearing assistant turn ends a run and starts a new single row', () => {
    const rows = groupMessageRuns([toolOnly('a1'), toolOnly('a2'), text('a3', 'summary'), toolOnly('a4'), toolOnly('a5')]);
    expect(rows.map((r) => r.kind)).toEqual(['run', 'single', 'run']);
  });

  it('a user message ends a run even mid-transcript', () => {
    const rows = groupMessageRuns([toolOnly('a1'), toolOnly('a2'), text('u1'), toolOnly('a3'), toolOnly('a4')]);
    expect(rows.map((r) => r.kind)).toEqual(['run', 'single', 'run']);
    expect((rows[2] as { key: string }).key).toBe('a3');
  });

  it('merges a run at the very start or end of the list', () => {
    expect(groupMessageRuns([toolOnly('a1'), toolOnly('a2')]).map((r) => r.kind)).toEqual(['run']);
    expect(groupMessageRuns([text('u1'), toolOnly('a1'), toolOnly('a2')]).map((r) => r.kind)).toEqual(['single', 'run']);
  });

  it('is stable-keyed: the run key is the first message id, unaffected by more turns joining', () => {
    const before = groupMessageRuns([toolOnly('a1'), toolOnly('a2')]);
    const after = groupMessageRuns([toolOnly('a1'), toolOnly('a2'), toolOnly('a3')]);
    expect((before[0] as { key: string }).key).toBe('a1');
    expect((after[0] as { key: string }).key).toBe('a1');
  });
});

describe('ChatView.transcriptRows (F6-1c)', () => {
  let view: ChatView;

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [ChatView],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    view = TestBed.createComponent(ChatView).componentInstance;
  });

  it('merges runs within the rendered (windowed) slice', () => {
    view.messages.set([text('u1'), toolOnly('a1'), toolOnly('a2'), text('a3', 'done')]);
    const rows = view.transcriptRows();
    expect(rows.map((r) => r.kind)).toEqual(['single', 'run', 'single']);
  });

  it('may split a run at the "Load earlier" window edge - the window wins, not the run', () => {
    // MESSAGE_WINDOW newest messages are rendered; build a run that straddles
    // the boundary so only its tail is in the initial window.
    const history: Message[] = [text('u0')];
    for (let i = 0; i < MESSAGE_WINDOW + 5; i++) {
      history.push(toolOnly(`a${i}`));
    }
    view.messages.set(history);

    // Initial window keeps only the newest MESSAGE_WINDOW messages, i.e. the
    // run's tail; its earlier part ("u0" and the first 5 tool-only turns) is
    // hidden, so the visible run is shorter than the full run in `messages`.
    const rows = view.transcriptRows();
    expect(rows.length).toBe(1);
    expect(rows[0].kind).toBe('run');
    if (rows[0].kind === 'run') {
      expect(rows[0].messages.length).toBe(MESSAGE_WINDOW);
      expect(rows[0].key).toBe('a5');
    }

    // After "Load earlier" the whole run (now including "u0") is visible again.
    view.loadEarlier();
    const grown = view.transcriptRows();
    expect(grown.map((r) => r.kind)).toEqual(['single', 'run']);
  });
});
