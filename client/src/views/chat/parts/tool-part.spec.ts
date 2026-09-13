/**
 * F6-1b: default expand state of a tool call.
 *
 * Every call - regardless of `toolIndex`, regardless of state - starts
 * collapsed; the only thing that opens a call by default is the "Expand tool
 * calls by default" preference, which opens every call at once. The header bar
 * is a non-interactive `<div class="tool-head">` holding two sibling controls:
 * a link (for delegation calls with a known child session) and a real
 * `<button class="head-toggle">` with `aria-expanded` that owns the empty
 * space and chevron - so clicking the bar's empty space or chevron toggles
 * the call, while the link navigates to the sub-agent session.
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

const TASK_COMPLETED: Part = {
  type: 'tool',
  id: 'tool-task-1',
  name: 'task',
  state: {
    state: 'completed',
    input: { agent: 'code', name: 'auth-audit' },
    output: 'done',
    title: 'task',
    structured: { taskID: 't1', name: 'auth-audit', agent: 'code', childSessionID: 'child-1' },
  },
};

const TASK_NO_CHILD: Part = {
  type: 'tool',
  id: 'tool-task-2',
  name: 'task',
  state: {
    state: 'completed',
    input: { agent: 'ask', name: 'research-x' },
    output: 'done',
    title: 'task',
    structured: { taskID: 't2', name: 'research-x', agent: 'ask', childSessionID: '' },
  },
};

const FLEET_COMPLETED: Part = {
  type: 'tool',
  id: 'tool-fleet-1',
  name: 'fleet',
  state: {
    state: 'completed',
    input: { agent: 'ask' },
    output: 'done',
    title: 'fleet',
    structured: { taskID: 'f1', name: 'fleet-child', agent: 'ask', childSessionID: 'fleet-child-1' },
  },
};

describe('ToolPartComponent default state (F6-1b)', () => {
  let fixture: ComponentFixture<ToolPartComponent>;
  let prefs: UiPrefsStore;

  /** The bar container — now a DIV, not a button. */
  function head(): HTMLElement {
    return fixture.nativeElement.querySelector('.tool-head');
  }

  /** The real toggle button inside the bar. */
  function toggle(): HTMLButtonElement {
    return fixture.nativeElement.querySelector('.head-toggle');
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
    expect(toggle().getAttribute('aria-expanded')).toBe('false');
    expect(head().classList.contains('collapsed')).toBeTrue();
    expect(fixture.nativeElement.querySelector('.tool-details')).toBeNull();

    fixture.componentRef.setInput('toolIndex', 3);
    await fixture.whenStable();
    expect(toggle().getAttribute('aria-expanded')).toBe('false');
    expect(head().classList.contains('collapsed')).toBeTrue();
    expect(fixture.nativeElement.querySelector('.tool-details')).toBeNull();
  });

  it('opens every call when the preference is on', async () => {
    prefs.setExpandToolCallsByDefault(true);
    fixture.componentRef.setInput('toolIndex', 5);
    await fixture.whenStable();
    expect(toggle().getAttribute('aria-expanded')).toBe('true');
  });

  it('toggles through the head-toggle button (native Enter/Space activation)', async () => {
    fixture.componentRef.setInput('toolIndex', 2);
    await fixture.whenStable();
    expect(toggle().tagName).toBe('BUTTON');
    expect(toggle().getAttribute('aria-expanded')).toBe('false');

    toggle().click();
    await fixture.whenStable();
    expect(toggle().getAttribute('aria-expanded')).toBe('true');
    expect(fixture.nativeElement.querySelector('.tool-details')).not.toBeNull();

    toggle().click();
    await fixture.whenStable();
    expect(toggle().getAttribute('aria-expanded')).toBe('false');
  });

  it('shows the state label as RUNNING/COMPLETED/FAILED (uppercased by CSS)', async () => {
    setPart(READ);
    await fixture.whenStable();
    expect(toggle().querySelector('.state-label')!.textContent!.trim()).toBe('completed');

    setPart(FETCH_RUNNING);
    await fixture.whenStable();
    expect(toggle().querySelector('.state-label')!.textContent!.trim()).toBe('running');

    setPart(FAILED);
    await fixture.whenStable();
    expect(toggle().querySelector('.state-label')!.textContent!.trim()).toBe('failed');
  });

  it('previews the shell command for bash', async () => {
    setPart(BASH);
    await fixture.whenStable();
    expect(toggle().querySelector('.tool-args')!.textContent).toContain('npm test -- --watch=false');
  });

  it('previews the path for a file tool', async () => {
    setPart(READ);
    await fixture.whenStable();
    expect(toggle().querySelector('.tool-args')!.textContent).toContain('src/main.ts');
  });

  it('previews the url for fetch', async () => {
    setPart(FETCH_RUNNING);
    await fixture.whenStable();
    expect(toggle().querySelector('.tool-args')!.textContent).toContain('https://example.com/data.json');
  });

  it('falls back to the first string argument for other tools', async () => {
    setPart(OTHER);
    await fixture.whenStable();
    expect(toggle().querySelector('.tool-args')!.textContent).toContain('TODO');
  });

  it('appends the output size to a completed call, but not to a running one', async () => {
    setPart(READ);
    await fixture.whenStable();
    expect(toggle().querySelector('.tool-args')!.textContent).toContain('· 8 B');

    setPart(FETCH_RUNNING);
    await fixture.whenStable();
    expect(toggle().querySelector('.tool-args')!.textContent).not.toContain('·');
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
    toggle().click();
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
    toggle().click();
    await fixture.whenStable();
    const errorPanel = fixture.nativeElement.querySelector('.panel.error code') as HTMLElement;
    expect(errorPanel.querySelector('script')).toBeNull();
    expect(errorPanel.textContent).toBe('boom <script>alert(1)</script>');
  });
});

describe('ToolPartComponent delegation bar (task link + empty-space toggle)', () => {
  let fixture: ComponentFixture<ToolPartComponent>;
  let prefs: UiPrefsStore;

  function head(): HTMLElement {
    return fixture.nativeElement.querySelector('.tool-head');
  }

  function toggle(): HTMLButtonElement {
    return fixture.nativeElement.querySelector('.head-toggle');
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
  });

  afterEach(() => {
    prefs.setExpandToolCallsByDefault(false);
  });

  it('task call with known child id: renders a head-target link with correct href and title, sibling of toggle', async () => {
    setPart(TASK_COMPLETED);
    fixture.componentRef.setInput('taskLinks', new Map([['auth-audit', 'child-1']]));
    await fixture.whenStable();

    const link = head().querySelector('a.head-target') as HTMLAnchorElement;
    expect(link).not.toBeNull();
    expect(link.getAttribute('href')).toBe('/chat/child-1');
    expect(link.getAttribute('title')).toBe('Open sub-agent session');
    expect(link.textContent).toContain('auth-audit');
    // The link contains the label cluster (dot + tool-name + delegation badge).
    expect(link.querySelector('.dot')).not.toBeNull();
    expect(link.querySelector('.tool-name')!.textContent!.trim()).toBe('task');
    expect(link.querySelector('.badge.delegation')).not.toBeNull();

    // The toggle button is a sibling of the link, not nested inside it.
    const children = [...head().children];
    const linkIdx = children.indexOf(link);
    const toggleBtn = toggle();
    const toggleIdx = children.indexOf(toggleBtn);
    expect(linkIdx).not.toBe(-1);
    expect(toggleIdx).not.toBe(-1);
    expect(linkIdx).not.toBe(toggleIdx);

    // No .task-row exists anywhere.
    expect(fixture.nativeElement.querySelector('.task-row')).toBeNull();
  });

  it('task call with no known child id: no link, but a muted target-name label and toggle still works', async () => {
    setPart(TASK_NO_CHILD);
    await fixture.whenStable();

    // No navigational link.
    expect(head().querySelector('a.head-target')).toBeNull();

    // A muted label shows the task name.
    const muted = head().querySelector('.target-name.muted') as HTMLElement;
    expect(muted).not.toBeNull();
    expect(muted.textContent!.trim()).toBe('research-x');

    // The toggle button is still present with the label cluster.
    expect(toggle()).not.toBeNull();
    expect(toggle().querySelector('.tool-name')!.textContent!.trim()).toBe('task');
    expect(toggle().querySelector('.badge.delegation')).not.toBeNull();

    // Clicking the toggle expands the call.
    toggle().click();
    await fixture.whenStable();
    expect(toggle().getAttribute('aria-expanded')).toBe('true');
    expect(fixture.nativeElement.querySelector('.tool-details')).not.toBeNull();

    // No .task-row exists anywhere.
    expect(fixture.nativeElement.querySelector('.task-row')).toBeNull();
  });

  it('fleet call: no head-target link (fleet fans out to many children)', async () => {
    setPart(FLEET_COMPLETED);
    await fixture.whenStable();

    expect(head().querySelector('a.head-target')).toBeNull();
    // No muted target-name either (fleet is not a task).
    expect(head().querySelector('.target-name')).toBeNull();
  });

  it('empty space inside the toggle button collapses/expands the call', async () => {
    setPart(READ);
    await fixture.whenStable();

    const space = toggle().querySelector('.head-space') as HTMLSpanElement;
    expect(space).not.toBeNull();
    expect(toggle().getAttribute('aria-expanded')).toBe('false');
    expect(head().classList.contains('collapsed')).toBeTrue();

    // Click the empty space — the click bubbles to the toggle button.
    space.click();
    await fixture.whenStable();
    expect(toggle().getAttribute('aria-expanded')).toBe('true');
    expect(head().classList.contains('collapsed')).toBeFalse();
    expect(fixture.nativeElement.querySelector('.tool-details')).not.toBeNull();

    // Click again to collapse.
    space.click();
    await fixture.whenStable();
    expect(toggle().getAttribute('aria-expanded')).toBe('false');
    expect(head().classList.contains('collapsed')).toBeTrue();
    expect(fixture.nativeElement.querySelector('.tool-details')).toBeNull();
  });
});
