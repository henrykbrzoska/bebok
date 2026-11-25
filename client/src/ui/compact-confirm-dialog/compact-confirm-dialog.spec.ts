/**
 * Compaction confirmation: the modal shows the preview, resolves the user's
 * choice (compact / cancel / send-without), Escape cancels, and the
 * "send without compacting" action exists only in suggest mode.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import type { CompactPreviewResponse } from '../../core/engine.dtos';
import { I18nService } from '../../i18n/i18n.service';
import { CompactConfirmDialog, formatCompactTokens } from './compact-confirm-dialog';

const PREVIEW: CompactPreviewResponse = {
  cutoff: 2,
  total: 5,
  kept: 3,
  before: 170_000,
  after: 30_000,
  messages: [
    { index: 0, role: 'user', excerpt: 'first question', tokens: 1_200 },
    { index: 1, role: 'assistant', excerpt: 'first answer', tokens: 4_800 },
    { index: 2, role: 'user', excerpt: 'second question', tokens: 300 },
    { index: 3, role: 'assistant', excerpt: 'second answer', tokens: 900 },
    { index: 4, role: 'user', excerpt: 'third question', tokens: 150 },
  ],
};

describe('CompactConfirmDialog', () => {
  let fixture: ComponentFixture<CompactConfirmDialog>;
  let component: CompactConfirmDialog;

  function root(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  function button(testid: string): HTMLButtonElement | null {
    return root().querySelector<HTMLButtonElement>(`[data-testid="${testid}"]`);
  }

  beforeEach(async () => {
    await TestBed.configureTestingModule({
      imports: [CompactConfirmDialog],
      providers: [provideZonelessChangeDetection(), I18nService],
    }).compileComponents();

    fixture = TestBed.createComponent(CompactConfirmDialog);
    component = fixture.componentInstance;
    fixture.detectChanges();
  });

  afterEach(() => {
    fixture.destroy();
  });

  it('renders nothing while closed', () => {
    expect(root().querySelector('[data-testid="compact-confirm"]')).toBeNull();
  });

  it('shows the title, summary and fork note for the preview', () => {
    void component.open(PREVIEW, 'manual');
    fixture.detectChanges();

    const title = root().querySelector('[data-testid="compact-confirm-title"]')?.textContent ?? '';
    expect(title).toContain('Context compaction: 170k → ~30k tokens');

    const summary = root().querySelector('[data-testid="compact-confirm-summary"]')?.textContent ?? '';
    expect(summary).toContain('2 earlier messages will be summarized');
    expect(summary).toContain('the last 3 stay unchanged');

    expect(root().textContent).toContain('Compaction creates a new session (fork)');
    component.choose('cancel');
  });

  it('lists every preview message with role, excerpt and tokens, marking the summarized ones', () => {
    void component.open(PREVIEW, 'manual');
    fixture.detectChanges();

    const items = root().querySelectorAll<HTMLElement>('[data-testid="compact-confirm-message"]');
    expect(items.length).toBe(5);
    expect(items[0].textContent).toContain('first question');
    expect(items[0].textContent).toContain('1k');
    expect(items[0].classList.contains('summarized')).toBeTrue();
    expect(items[1].classList.contains('summarized')).toBeTrue();
    expect(items[2].classList.contains('summarized')).toBeFalse();
    component.choose('cancel');
  });

  it('resolves "compact" from the confirm button', async () => {
    const outcome = component.open(PREVIEW, 'manual');
    fixture.detectChanges();

    button('compact-confirm-compact')!.click();

    expect(await outcome).toBe('compact');
    fixture.detectChanges();
    expect(root().querySelector('[data-testid="compact-confirm"]')).toBeNull();
  });

  it('resolves "cancel" from the cancel button', async () => {
    const outcome = component.open(PREVIEW, 'manual');
    fixture.detectChanges();

    button('compact-confirm-cancel')!.click();

    expect(await outcome).toBe('cancel');
  });

  it('resolves "cancel" on Escape', async () => {
    const outcome = component.open(PREVIEW, 'manual');
    fixture.detectChanges();

    const dialog = root().querySelector<HTMLElement>('[role="dialog"]')!;
    dialog.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));

    expect(await outcome).toBe('cancel');
  });

  it('offers "send without compacting" only in suggest mode', async () => {
    void component.open(PREVIEW, 'manual');
    fixture.detectChanges();
    expect(button('compact-confirm-send-without')).toBeNull();
    component.choose('cancel');

    const outcome = component.open(PREVIEW, 'suggest');
    fixture.detectChanges();
    const sendWithout = button('compact-confirm-send-without');
    expect(sendWithout).not.toBeNull();

    sendWithout!.click();
    expect(await outcome).toBe('send-without');
  });

  it('resolves the previous confirmation as "cancel" when a new one opens', async () => {
    const first = component.open(PREVIEW, 'suggest');
    const second = component.open(PREVIEW, 'manual');

    expect(await first).toBe('cancel');
    component.choose('compact');
    expect(await second).toBe('compact');
  });

  it('close() resolves "cancel" without a user choice', async () => {
    const outcome = component.open(PREVIEW, 'suggest');
    component.close();
    expect(await outcome).toBe('cancel');
  });
});

describe('formatCompactTokens', () => {
  it('formats counts like the toolbar meter', () => {
    expect(formatCompactTokens(950)).toBe('950');
    expect(formatCompactTokens(42_000)).toBe('42k');
    expect(formatCompactTokens(1_000_000)).toBe('1M');
    expect(formatCompactTokens(1_200_000)).toBe('1.2M');
  });
});
