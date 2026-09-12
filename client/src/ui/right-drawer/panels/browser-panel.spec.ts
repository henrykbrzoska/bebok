/**
 * WP-BROWSER (F6-19): the Browser panel projects the newest
 * `browser_screenshot` image and `browser_*` URL out of the session
 * transcript and offers an outbound "Open in my browser" link.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { EngineClient } from '../../../core/engine-client.service';
import { Message, SessionMeta } from '../../../core/engine.dtos';
import { EventsStore } from '../../../core/events.store';
import { ChatSessionStore } from '../../../views/chat/chat-session.store';
import { BrowserPanel, isLinkable, latestLocation, latestScreenshot } from './browser-panel';

const META: SessionMeta = {
  id: 's1',
  directory: '/p',
  agent: 'code',
  created_at: 0,
  updated_at: 0,
  usage: { input_tokens: 0, output_tokens: 0 },
} as SessionMeta;

const PNG = 'iVBORw0KGgo=';

function user(text: string): Message {
  return { id: 'u', role: 'user', parts: [{ type: 'text', text }] };
}

function openMessage(url: string, title = 'Example'): Message {
  return {
    id: 'a1',
    role: 'assistant',
    parts: [
      {
        type: 'tool',
        id: 'c1',
        name: 'browser_open',
        state: {
          state: 'completed',
          input: { url },
          output: `Opened ${url}`,
          title: 'browser_open',
          structured: { url, title } as never,
        },
      },
    ],
  };
}

function screenshotMessage(url: string, data = PNG, id = 'c2'): Message {
  return {
    id: 'a2',
    role: 'assistant',
    parts: [
      {
        type: 'tool',
        id,
        name: 'browser_screenshot',
        state: {
          state: 'completed',
          input: {},
          output: `Screenshot of ${url}`,
          title: 'browser_screenshot',
          structured: { url, title: 'Example', media_type: 'image/png' } as never,
        },
      },
      { type: 'image', media_type: 'image/png', data, name: `browser_screenshot:${id}` },
      { type: 'text', text: 'Here is the page.' },
    ],
  };
}

describe('BrowserPanel helpers (F6-19)', () => {
  it('finds the newest screenshot paired with its tool part', () => {
    const messages = [
      user('go'),
      screenshotMessage('https://old.example', 'OLD=', 'c1'),
      screenshotMessage('https://new.example', 'NEW=', 'c9'),
    ];
    const shot = latestScreenshot(messages);
    expect(shot).not.toBeNull();
    expect(shot!.data).toBe('NEW=');
    expect(shot!.mediaType).toBe('image/png');
    expect(shot!.url).toBe('https://new.example');
  });

  it('ignores images that do not follow a browser_screenshot tool part', () => {
    const messages: Message[] = [
      {
        id: 'u1',
        role: 'user',
        parts: [
          { type: 'text', text: 'look' },
          { type: 'image', media_type: 'image/png', data: 'USER=' },
        ],
      },
      {
        id: 'a1',
        role: 'assistant',
        parts: [
          {
            type: 'tool',
            id: 'c1',
            name: 'browser_screenshot',
            state: { state: 'error', input: {}, error: 'error: no page' },
          },
        ],
      },
    ];
    expect(latestScreenshot(messages)).toBeNull();
  });

  it('reports the newest browser_* URL and title', () => {
    const messages = [user('go'), openMessage('https://a.example', 'A')];
    const loc = latestLocation(messages);
    expect(loc).toEqual({ url: 'https://a.example', title: 'A', tool: 'browser_open' });
    expect(latestLocation([user('nothing')])).toBeNull();
  });

  it('surfaces a failed browser_* call as an error location', () => {
    const messages: Message[] = [
      openMessage('https://a.example'),
      {
        id: 'a3',
        role: 'assistant',
        parts: [
          {
            type: 'tool',
            id: 'c3',
            name: 'browser_click',
            state: { state: 'error', input: { selector: '#x' }, error: 'no element matches' },
          },
        ],
      },
    ];
    const loc = latestLocation(messages);
    expect(loc?.tool).toBe('browser_click');
    expect(loc?.error).toBe('no element matches');
  });

  it('only links http(s) URLs', () => {
    expect(isLinkable('https://example.com')).toBeTrue();
    expect(isLinkable('http://127.0.0.1:8798/')).toBeTrue();
    expect(isLinkable('file:///etc/passwd')).toBeFalse();
    expect(isLinkable('data:text/html,<b>x</b>')).toBeFalse();
    expect(isLinkable('')).toBeFalse();
  });
});

describe('BrowserPanel (F6-19)', () => {
  let fixture: ComponentFixture<BrowserPanel>;
  let session: ChatSessionStore;

  beforeEach(() => {
    localStorage.clear();
    const events = { onEvent: jasmine.createSpy('onEvent').and.returnValue(() => undefined) };
    TestBed.configureTestingModule({
      imports: [BrowserPanel],
      providers: [
        provideZonelessChangeDetection(),
        provideRouter([]),
        { provide: EngineClient, useValue: {} },
        { provide: EventsStore, useValue: events },
      ],
    });
    session = TestBed.inject(ChatSessionStore);
    fixture = TestBed.createComponent(BrowserPanel);
    fixture.detectChanges();
  });

  afterEach(() => fixture.destroy());

  async function settle(): Promise<void> {
    await fixture.whenStable();
    fixture.detectChanges();
  }

  function el(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  it('shows the no-session hint without a session', () => {
    expect(el().textContent).toContain('Open a session');
  });

  it('shows the empty state when the transcript has no browser_* calls', async () => {
    session.meta.set(META);
    session.messages.set([user('hello')]);
    await settle();
    expect(el().textContent).toContain('No browser activity');
    expect(el().querySelector('[data-testid="browser-screenshot"]')).toBeNull();
  });

  it('renders the URL, the outbound link and the screenshot from the transcript', async () => {
    session.meta.set(META);
    session.messages.set([
      user('open it'),
      openMessage('https://example.com/', 'Example Domain'),
      screenshotMessage('https://example.com/'),
    ]);
    await settle();
    const link = el().querySelector<HTMLAnchorElement>('[data-testid="browser-open-link"]');
    expect(link).not.toBeNull();
    expect(link!.getAttribute('href')).toBe('https://example.com/');
    expect(link!.getAttribute('target')).toBe('_blank');
    expect(link!.getAttribute('rel')).toContain('noopener');
    expect(link!.textContent).toContain('Open in my browser');
    expect(el().textContent).toContain('Example Domain');
    const img = el().querySelector<HTMLImageElement>('[data-testid="browser-screenshot"]');
    expect(img).not.toBeNull();
    expect(img!.getAttribute('src')).toBe(`data:image/png;base64,${PNG}`);
  });

  it('updates when a new screenshot lands in the transcript (SSE-patched messages)', async () => {
    session.meta.set(META);
    session.messages.set([openMessage('https://example.com/')]);
    await settle();
    expect(el().querySelector('[data-testid="browser-screenshot"]')).toBeNull();
    expect(el().textContent).toContain('No screenshot yet');

    session.messages.update((m) => [...m, screenshotMessage('https://example.com/', 'NEW=')]);
    await settle();
    const img = el().querySelector<HTMLImageElement>('[data-testid="browser-screenshot"]');
    expect(img!.getAttribute('src')).toBe('data:image/png;base64,NEW=');
  });

  it('does not offer a link for non-http URLs', async () => {
    session.meta.set(META);
    session.messages.set([openMessage('file:///C:/x.html', 'local')]);
    await settle();
    expect(el().textContent).toContain('file:///C:/x.html');
    expect(el().querySelector('[data-testid="browser-open-link"]')).toBeNull();
  });
});
