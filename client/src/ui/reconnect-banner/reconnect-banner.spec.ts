/**
 * E2E follow-up: after an engine restart the client's token is stale and
 * every request answers 401. The banner must appear as soon as
 * `EngineClient.unauthorized` flips, stay until a reconnect succeeds, and
 * hand the pasted `BEBOK_READY` address to `EngineClient.reconnect()`.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { EngineClient } from '../../core/engine-client.service';
import { EventsStore } from '../../core/events.store';
import { ReconnectBanner } from './reconnect-banner';

describe('ReconnectBanner', () => {
  let fixture: ComponentFixture<ReconnectBanner>;
  let unauthorized: ReturnType<typeof signal<boolean>>;
  let reconnect: jasmine.Spy;
  let restart: jasmine.Spy;

  function make(isTauri = false): void {
    unauthorized = signal(false);
    reconnect = jasmine.createSpy('reconnect').and.callFake(async () => {
      unauthorized.set(false);
    });
    restart = jasmine.createSpy('restart');
    TestBed.resetTestingModule();
    TestBed.configureTestingModule({
      imports: [ReconnectBanner],
      providers: [
        provideZonelessChangeDetection(),
        {
          provide: EngineClient,
          useValue: {
            unauthorized,
            isTauri: signal(isTauri),
            remoteDefaults: () => ({ baseUrl: 'http://127.0.0.1:8787' }),
            reconnect,
          },
        },
        { provide: EventsStore, useValue: { restart } },
      ],
    });
    fixture = TestBed.createComponent(ReconnectBanner);
    fixture.detectChanges();
  }

  function banner(): HTMLElement | null {
    return (fixture.nativeElement as HTMLElement).querySelector('[data-testid="reconnect-banner"]');
  }

  it('stays hidden while the token is accepted', () => {
    make();
    expect(banner()).toBeNull();
  });

  it('appears once the engine rejects the token and offers the address form', async () => {
    make();
    unauthorized.set(true);
    await fixture.whenStable();
    expect(banner()).not.toBeNull();
    expect(banner()!.textContent).toContain('Engine restarted or token changed');
    const input = banner()!.querySelector('[data-testid="reconnect-address"]') as HTMLInputElement;
    expect(input).not.toBeNull();
    expect(input.value).toBe('http://127.0.0.1:8787');
  });

  it('reconnects with the pasted BEBOK_READY address and restarts the event stream', async () => {
    make();
    unauthorized.set(true);
    await fixture.whenStable();
    fixture.componentInstance.address.set('http://127.0.0.1:9000/?token=abc');
    await fixture.componentInstance.reconnect();
    await fixture.whenStable();

    expect(reconnect).toHaveBeenCalledWith('http://127.0.0.1:9000/?token=abc');
    expect(restart).toHaveBeenCalled();
    expect(banner()).toBeNull();
  });

  it('keeps the banner up with an error when the engine still answers 401', async () => {
    make();
    unauthorized.set(true);
    await fixture.whenStable();
    reconnect.and.callFake(async () => {
      unauthorized.set(true);
      throw new Error('engine rejected the token (401)');
    });

    await fixture.componentInstance.reconnect();
    await fixture.whenStable();

    expect(banner()).not.toBeNull();
    expect(banner()!.querySelector('[role="alert"]')?.textContent).toContain('Still rejected');
    expect(restart).not.toHaveBeenCalled();
  });

  it('on the desktop shell re-reads the sidecar address instead of asking for one', async () => {
    make(true);
    unauthorized.set(true);
    await fixture.whenStable();
    expect(banner()!.querySelector('[data-testid="reconnect-address"]')).toBeNull();

    await fixture.componentInstance.reconnect();
    expect(reconnect).toHaveBeenCalledWith(undefined);
  });
});
