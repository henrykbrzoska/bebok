/**
 * F9-7: the status part renders a single-line system row whose dot colour
 * follows the kind (started / progress / ended) and the failure words.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { StatusPart } from '../../../core/engine.dtos';
import { StatusPartComponent, statusTone } from './status-part';

function status(kind: string, text: string, extra: Partial<StatusPart> = {}): StatusPart {
  return { type: 'status', kind, text, at: 1_700_000_000_000, ...extra };
}

describe('statusTone (F9-7)', () => {
  it('colours started=accent, progress=muted, ended=success unless the text reports a failure', () => {
    expect(statusTone('task.started', 'api-orders started (code · openai/gpt-5.6-luna)')).toBe('started');
    expect(statusTone('task.progress', 'api-orders: edit_file · 12 calls · 41k tok')).toBe('progress');
    expect(statusTone('task.ended', 'api-orders finished in 4m20s · 151k tok · 3 files changed')).toBe('success');
    expect(statusTone('task.ended', 'api-orders failed after 12s')).toBe('danger');
    expect(statusTone('task.ended', 'api-orders aborted')).toBe('danger');
    expect(statusTone('something.else', 'x')).toBe('muted');
  });
});

describe('StatusPartComponent (F9-7)', () => {
  let fixture: ComponentFixture<StatusPartComponent>;

  function root(): HTMLElement {
    return fixture.nativeElement as HTMLElement;
  }

  beforeEach(() => {
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [StatusPartComponent],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    fixture = TestBed.createComponent(StatusPartComponent);
  });

  it('renders the text as one compact row with a kind-coloured dot', async () => {
    fixture.componentRef.setInput('part', status('task.started', 'api-orders started (code · openai/gpt-5.6-luna)'));
    await fixture.whenStable();

    const row = root().querySelector<HTMLElement>('[data-testid="status-part"]');
    expect(row).not.toBeNull();
    expect(row!.textContent).toContain('api-orders started (code · openai/gpt-5.6-luna)');
    expect(row!.getAttribute('data-kind')).toBe('task.started');
    expect(row!.getAttribute('data-tone')).toBe('started');
    expect(row!.classList.contains('tone-started')).toBeTrue();
    expect(row!.querySelector('.dot')).not.toBeNull();
    // No link without a child session id.
    expect(row!.querySelector('a')).toBeNull();
  });

  it('marks a failed ending as danger and links to the child session when known', async () => {
    fixture.componentRef.setInput(
      'part',
      status('task.ended', 'api-orders failed: boom', { childSessionID: 'child-1', name: 'api-orders' }),
    );
    await fixture.whenStable();

    const row = root().querySelector<HTMLElement>('[data-testid="status-part"]')!;
    expect(row.getAttribute('data-tone')).toBe('danger');
    const link = row.querySelector<HTMLAnchorElement>('a.text');
    expect(link).not.toBeNull();
    expect(link!.getAttribute('href')).toContain('/chat/child-1');
  });
});
