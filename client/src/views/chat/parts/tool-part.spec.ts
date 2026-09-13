/**
 * F6-1b: default expand state of a tool call.
 *
 * Every call - regardless of `toolIndex`, regardless of state - starts
 * collapsed; the only thing that opens a call by default is the "Expand tool
 * calls by default" preference, which opens every call at once. The header is
 * a real `<button>` with `aria-expanded`, so keyboard activation (Enter/Space)
 * is native.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { Part, ToolSafetyEntry } from '../../../core/engine.dtos';
import { ToolSafetyStore } from '../../../core/tool-safety.store';
import { UiPrefsStore } from '../../../core/ui-prefs.store';
import { ToolPartComponent } from './tool-part';

function entry(name: string, category: ToolSafetyEntry['category']): ToolSafetyEntry {
  return { name, source: 'built-in', category, default_category: category, is_override: false };
}

const READ: Part = {
  type: 'tool',
  id: 'tool-1',
  name: 'read_file',
  state: { state: 'completed', input: { path: 'src/main.ts' }, output: 'contents', title: 'read_file' },
};

const BASH: Part = {
  type: 'tool',
  id: 'tool-2',
  name: 'bash',
  state: { state: 'completed', input: { command: 'npm test -- --watch=false' }, output: 'ok', title: 'bash' },
};

const FETCH_RUNNING: Part = {
  type: 'tool',
  id: 'tool-3',
  name: 'fetch',
  state: { state: 'running', input: { url: 'https://example.com/data.json' }, started_at: 1 },
};

const OTHER: Part = {
  type: 'tool',
  id: 'tool-4',
  name: 'grep',
  state: { state: 'completed', input: { pattern: 'TODO', path: 'src' }, output: 'match', title: 'grep' },
};

const FAILED: Part = {
  type: 'tool',
  id: 'tool-5',
  name: 'bash',
  state: { state: 'error', input: { command: 'exit 1' }, error: 'boom' },
};

describe('ToolPartComponent default state (F6-1b)', () => {
  let fixture: ComponentFixture<ToolPartComponent>;
  let prefs: UiPrefsStore;

  function head(): HTMLButtonElement {
    return fixture.nativeElement.querySelector('.tool-head');
  }

  function setPart(part: Part): void {
    fixture.componentRef.setInput('part', part);
  }

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [ToolPartComponent],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    prefs = TestBed.inject(UiPrefsStore);
    prefs.setExpandToolCallsByDefault(false);
    fixture = TestBed.createComponent(ToolPartComponent);
    setPart(READ);
  });

  afterEach(() => {
    prefs.setExpandToolCallsByDefault(false);
  });

  it('starts every call collapsed when the preference is off, including toolIndex 0 (no "first call" exception)', async () => {
    fixture.componentRef.setInput('toolIndex', 0);
    await fixture.whenStable();
    expect(head().getAttribute('aria-expanded')).toBe('false');
    expect(head().classList.contains('collapsed')).toBeTrue();
    expect(fixture.nativeElement.querySelector('.tool-details')).toBeNull();

    fixture.componentRef.setInput('toolIndex', 3);
    await fixture.whenStable();
    expect(head().getAttribute('aria-expanded')).toBe('false');
    expect(head().classList.contains('collapsed')).toBeTrue();
    expect(fixture.nativeElement.querySelector('.tool-details')).toBeNull();
  });

  it('opens every call when the preference is on', async () => {
    prefs.setExpandToolCallsByDefault(true);
    fixture.componentRef.setInput('toolIndex', 5);
    await fixture.whenStable();
    expect(head().getAttribute('aria-expanded')).toBe('true');
  });

  it('toggles through the header button (native Enter/Space activation)', async () => {
    fixture.componentRef.setInput('toolIndex', 2);
    await fixture.whenStable();
    expect(head().tagName).toBe('BUTTON');
    expect(head().getAttribute('aria-expanded')).toBe('false');

    head().click();
    await fixture.whenStable();
    expect(head().getAttribute('aria-expanded')).toBe('true');
    expect(fixture.nativeElement.querySelector('.tool-details')).not.toBeNull();

    head().click();
    await fixture.whenStable();
    expect(head().getAttribute('aria-expanded')).toBe('false');
  });

  it('shows the state label as RUNNING/COMPLETED/FAILED (uppercased by CSS)', async () => {
    setPart(READ);
    await fixture.whenStable();
    expect(head().querySelector('.state-label')!.textContent!.trim()).toBe('completed');

    setPart(FETCH_RUNNING);
    await fixture.whenStable();
    expect(head().querySelector('.state-label')!.textContent!.trim()).toBe('running');

    setPart(FAILED);
    await fixture.whenStable();
    expect(head().querySelector('.state-label')!.textContent!.trim()).toBe('failed');
  });

  it('previews the shell command for bash', async () => {
    setPart(BASH);
    await fixture.whenStable();
    expect(head().querySelector('.tool-args')!.textContent).toContain('npm test -- --watch=false');
  });

  it('previews the path for a file tool', async () => {
    setPart(READ);
    await fixture.whenStable();
    expect(head().querySelector('.tool-args')!.textContent).toContain('src/main.ts');
  });

  it('previews the url for fetch', async () => {
    setPart(FETCH_RUNNING);
    await fixture.whenStable();
    expect(head().querySelector('.tool-args')!.textContent).toContain('https://example.com/data.json');
  });

  it('falls back to the first string argument for other tools', async () => {
    setPart(OTHER);
    await fixture.whenStable();
    expect(head().querySelector('.tool-args')!.textContent).toContain('TODO');
  });

  it('appends the output size to a completed call, but not to a running one', async () => {
    setPart(READ);
    await fixture.whenStable();
    expect(head().querySelector('.tool-args')!.textContent).toContain('· 8 B');

    setPart(FETCH_RUNNING);
    await fixture.whenStable();
    expect(head().querySelector('.tool-args')!.textContent).not.toContain('·');
  });

  it('F7-7: colours the header dot by the stamped safety category, not the run state', async () => {
    setPart({ ...READ, safety: 'safe' });
    await fixture.whenStable();
    let dot = head().querySelector('.dot')!;
    expect(dot.classList.contains('safety-safe')).toBeTrue();
    expect(dot.getAttribute('data-safety')).toBe('safe');
    expect(dot.classList.contains('ring-failed')).toBeFalse();

    setPart({ ...BASH, safety: 'dangerous', permission: 'allow', mutating: false });
    await fixture.whenStable();
    dot = head().querySelector('.dot')!;
    // The runtime verdict (auto-allowed) does not turn a dangerous tool green.
    expect(dot.classList.contains('safety-dangerous')).toBeTrue();
    expect(dot.classList.contains('safety-safe')).toBeFalse();

    setPart({ ...FETCH_RUNNING, safety: 'caution' });
    await fixture.whenStable();
    dot = head().querySelector('.dot')!;
    expect(dot.classList.contains('safety-caution')).toBeTrue();
    expect(dot.classList.contains('pulse')).toBeTrue();
  });

  it('F7-7: a historical part without a stamped category takes the current one by tool name', async () => {
    const store = TestBed.inject(ToolSafetyStore);
    setPart(READ);
    await fixture.whenStable();
    // Map not loaded yet: gray.
    expect(head().querySelector('.dot')!.classList.contains('safety-uncategorized')).toBeTrue();

    store.entries.set([entry('read_file', 'safe'), entry('bash', 'dangerous')]);
    await fixture.whenStable();
    expect(head().querySelector('.dot')!.classList.contains('safety-safe')).toBeTrue();

    setPart(BASH);
    await fixture.whenStable();
    expect(head().querySelector('.dot')!.classList.contains('safety-dangerous')).toBeTrue();

    // A tool the engine no longer lists stays gray.
    setPart({ ...OTHER, name: 'mcp__gone__thing' });
    await fixture.whenStable();
    expect(head().querySelector('.dot')!.classList.contains('safety-uncategorized')).toBeTrue();
  });

  it('F7-7: the dot tooltip carries the category and the colour legend', async () => {
    setPart({ ...READ, safety: 'dangerous' });
    await fixture.whenStable();
    const title = head().querySelector('.dot')!.getAttribute('title') ?? '';
    expect(title).toContain('dangerous');
    expect(title).toContain('legend');
    expect(title).toContain('green safe');
    expect(title).toContain('gray uncategorized');

    setPart({ ...READ, safety: 'uncategorized' });
    await fixture.whenStable();
    expect(head().querySelector('.dot')!.getAttribute('title')).toContain('Settings > Permissions');
  });

  it('F7-7: a failed call keeps a red ring regardless of its safety colour', async () => {
    setPart({ ...FAILED, safety: 'dangerous' });
    await fixture.whenStable();
    const dot = head().querySelector('.dot')!;
    expect(dot.classList.contains('ring-failed')).toBeTrue();
    expect(dot.classList.contains('safety-dangerous')).toBeTrue();
  });

  it('F9-11: linkifies a bare URL in the error panel (plain text, no markdown)', async () => {
    setPart({
      ...FAILED,
      state: { state: 'error', input: { command: 'curl' }, error: 'failed: see https://example.com/logs.' },
    });
    head().click();
    await fixture.whenStable();
    const errorPanel = fixture.nativeElement.querySelector('.panel.error code') as HTMLElement;
    const link = errorPanel.querySelector('a') as HTMLAnchorElement;
    expect(link.getAttribute('href')).toBe('https://example.com/logs');
    expect(link.getAttribute('target')).toBe('_blank');
    expect(link.getAttribute('rel')).toBe('noopener noreferrer');
    // The trailing period stays out of the link, and raw HTML in the error
    // message is escaped rather than rendered.
    expect(errorPanel.textContent).toBe('failed: see https://example.com/logs.');
  });

  it('F9-11: escapes raw HTML in the error panel instead of rendering it', async () => {
    setPart({
      ...FAILED,
      state: { state: 'error', input: { command: 'curl' }, error: 'boom <script>alert(1)</script>' },
    });
    head().click();
    await fixture.whenStable();
    const errorPanel = fixture.nativeElement.querySelector('.panel.error code') as HTMLElement;
    expect(errorPanel.querySelector('script')).toBeNull();
    expect(errorPanel.textContent).toBe('boom <script>alert(1)</script>');
  });
});
