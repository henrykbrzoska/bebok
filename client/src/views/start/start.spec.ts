/**
 * F0-1: the client always attempts to connect at startup.
 *
 * - a failing `fetch` must end in an explicit `error` phase with a visible
 *   Retry button and an editable address field (no silent "idle");
 * - a successful connection must end in `live` without ever showing the old
 *   idle/disconnected screen.
 */

import { provideZonelessChangeDetection } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';
import { provideRouter } from '@angular/router';

import { EventsStore } from '../../core/events.store';
import { StartView } from './start';

describe('StartView (F0-1 auto-connect)', () => {
  let fixture: ComponentFixture<StartView>;
  let originalFetch: typeof fetch;

  function setup(): void {
    TestBed.configureTestingModule({
      imports: [StartView],
      providers: [provideZonelessChangeDetection(), provideRouter([])],
    });
    fixture = TestBed.createComponent(StartView);
    // The SSE stream is irrelevant here - keep it from opening a real socket.
    const events = TestBed.inject(EventsStore);
    spyOn(events, 'start');
    spyOn(events, 'restart');
  }

  beforeEach(() => {
    originalFetch = window.fetch;
    localStorage.clear();
    TestBed.resetTestingModule();
  });

  afterEach(() => {
    window.fetch = originalFetch;
  });

  it('shows an explicit error state with a Retry button when the engine is unreachable', async () => {
    window.fetch = jasmine
      .createSpy('fetch')
      .and.returnValue(Promise.reject(new TypeError('Failed to fetch')));
    setup();

    await fixture.componentInstance.ngOnInit();
    await fixture.whenStable();
    fixture.detectChanges();

    const component = fixture.componentInstance;
    expect(component.phase()).toBe('error');
    expect(component.connected()).toBeFalse();
    expect(component.statusTone()).toBe('danger');
    // The attempted address is part of the message the user sees.
    expect(component.statusLabel()).toContain('127.0.0.1:8787');

    const html = (fixture.nativeElement as HTMLElement).textContent ?? '';
    expect(html).toContain('Retry');
    const input = (fixture.nativeElement as HTMLElement).querySelector('input');
    expect(input).withContext('address field must be editable after a failure').not.toBeNull();
  });

  it('reaches the live state without a flash of idle when the engine answers', async () => {
    const seen: string[] = [];
    window.fetch = jasmine
      .createSpy('fetch')
      .and.returnValue(Promise.resolve(new Response('{"sessions":[]}', { status: 200 })));
    setup();

    const component = fixture.componentInstance;
    seen.push(component.phase());
    await component.ngOnInit();
    await fixture.whenStable();
    seen.push(component.phase());

    expect(component.phase()).toBe('live');
    expect(component.error()).toBeNull();
    expect(component.statusTone()).toBe('success');
    // 'connecting' is the only intermediate phase - never a silent idle state.
    expect(seen[0]).toBe('connecting');
  });
});
