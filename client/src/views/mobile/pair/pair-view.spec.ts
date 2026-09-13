/**
 * WP-M6 (F10-22/F10-23): the pairing screen - a deep-link invite reaches the
 * confirmation unscanned, malformed input errors cleanly, and every pairing
 * outcome has its own sentence.
 */

import { provideZonelessChangeDetection, signal } from '@angular/core';
import { ComponentFixture, TestBed } from '@angular/core/testing';

import { ENGINE_API, EngineApi } from '../../../core/engine-api';
import { EngineTargetStore } from '../../../core/engine-target.store';
import { MemoryCacheStore, OFFLINE_CACHE_STORE } from '../../../core/remote/offline-cache';
import { PairError, PairErrorCode, parsePairUrl } from '../../../core/remote/pair-protocol';
import { PairingFlow } from '../../../core/remote/pairing';
import { QrScanner } from '../../../core/remote/qr-scanner';
import { PairView } from './pair-view';

describe('PairView (WP-M6)', () => {
  let fixture: ComponentFixture<PairView>;
  let pairSpy: jasmine.Spy;
  let switchSpy: jasmine.Spy;
  let flow: PairingFlow;

  beforeEach(() => {
    localStorage.clear();
    pairSpy = jasmine.createSpy('pairWithDesktop');
    switchSpy = jasmine.createSpy('switchTarget').and.callFake(async (id: string) => {
      TestBed.inject(EngineTargetStore).setActive(id);
      return TestBed.inject(EngineTargetStore).byId(id)!;
    });
    const engine = {
      connection: signal(null),
      unauthorized: signal(false),
      pairWithDesktop: pairSpy,
      switchTarget: switchSpy,
    } as unknown as EngineApi;
    TestBed.configureTestingModule({
      imports: [PairView],
      providers: [
        provideZonelessChangeDetection(),
        { provide: ENGINE_API, useValue: engine },
        { provide: OFFLINE_CACHE_STORE, useValue: new MemoryCacheStore() },
      ],
    });
    flow = TestBed.inject(PairingFlow);
    flow.probeFetch = jasmine.createSpy('fetch').and.callFake(
      async () => new Response(JSON.stringify({ engineName: 'rafal-pc', fingerprint: 'fp' }), { status: 200 }),
    );
    flow.probeTimeoutMs = 50;
    TestBed.inject(QrScanner).isNative = () => false;
    fixture = TestBed.createComponent(PairView);
    fixture.detectChanges();
  });

  afterEach(() => {
    fixture.destroy();
    localStorage.clear();
  });

  const el = (): HTMLElement => fixture.nativeElement as HTMLElement;

  async function settle(): Promise<void> {
    // Real macrotasks: `Response.json()` and the probe's fetch settle off the
    // microtask queue (jasmine's clock, where installed, only fakes timers).
    await new Promise((resolve) => setTimeout(resolve, 20));
    for (let i = 0; i < 4; i++) {
      await Promise.resolve();
    }
    fixture.detectChanges();
  }

  it('starts on the entry card; the scan button is hidden off-device', () => {
    expect(el().querySelector('[data-testid="pair-view"]')?.getAttribute('data-phase')).toBe('idle');
    expect(el().querySelector('[data-testid="pair-scan"]')).toBeNull();
    expect(el().querySelector('[data-testid="pair-manual"]')).not.toBeNull();
  });

  it('a deep-link invite reaches the confirmation without scanning', async () => {
    const invite = parsePairUrl('bebok://pair?v=1&ep=100.64.0.7:8790&code=ABCD2345&fp=fp');
    fixture.componentRef.setInput('invite', invite);
    fixture.detectChanges();
    await settle();
    expect(el().querySelector('[data-testid="pair-view"]')?.getAttribute('data-phase')).toBe('confirm');
    expect(el().querySelector('[data-testid="pair-confirm-title"]')?.textContent).toContain('rafal-pc');
    expect(el().querySelector('[data-testid="pair-confirm-endpoint"]')?.textContent).toContain('http://100.64.0.7:8790');
  });

  it('malformed manual input errors cleanly without touching the network', async () => {
    const view = fixture.componentInstance;
    view.code.set('abc');
    view.endpoint.set('100.64.0.7:8790');
    view.submitManual();
    fixture.detectChanges();
    expect(el().querySelector('[data-testid="pair-manual-error"]')?.textContent).toContain('8');
    view.code.set('ABCD2345');
    view.endpoint.set('http://8.8.8.8:1');
    view.submitManual();
    fixture.detectChanges();
    expect(el().querySelector('[data-testid="pair-manual-error"]')?.textContent).toContain('private');
    view.endpoint.set('::not a host::');
    view.submitManual();
    fixture.detectChanges();
    expect(el().querySelector('[data-testid="pair-manual-error"]')).not.toBeNull();
    expect(flow.probeFetch).not.toHaveBeenCalled();
  });

  it('a malformed scanned URL shows the parse reason', () => {
    fixture.componentInstance.acceptUrl('https://example.com/?code=X');
    fixture.detectChanges();
    expect(el().querySelector('[data-testid="pair-scan-error"]')?.textContent).toContain('not a Bebok pairing link');
  });

  it('manual code + address -> confirm -> pair -> paired output', async () => {
    pairSpy.and.resolveTo({ deviceId: 'd1', token: 't', engineName: 'rafal-pc', fingerprint: 'fp' });
    const paired: string[] = [];
    fixture.componentInstance.paired.subscribe((id) => paired.push(id));
    const view = fixture.componentInstance;
    view.code.set('abcd-2345');
    view.endpoint.set('192.168.1.20:8790');
    view.submitManual();
    await settle();
    expect(el().querySelector('[data-testid="pair-view"]')?.getAttribute('data-phase')).toBe('confirm');
    el().querySelector<HTMLButtonElement>('[data-testid="pair-confirm"]')!.click();
    await settle();
    expect(pairSpy).toHaveBeenCalledWith('http://192.168.1.20:8790', 'ABCD2345', 'My phone', jasmine.anything());
    expect(switchSpy).toHaveBeenCalledTimes(1);
    expect(paired).toEqual(['desktop:d1']);
    expect(el().querySelector('[data-testid="pair-done"]')?.textContent).toContain('rafal-pc');
  });

  /** `settle()` under an installed jasmine clock (setTimeout is faked). */
  async function settleFaked(): Promise<void> {
    for (let round = 0; round < 3; round++) {
      jasmine.clock().tick(25);
      for (let i = 0; i < 6; i++) {
        await Promise.resolve();
      }
    }
    fixture.detectChanges();
  }

  it('shows the elapsed time while the desktop is asked', async () => {
    pairSpy.and.returnValue(new Promise(() => undefined));
    fixture.componentRef.setInput('invite', parsePairUrl('bebok://pair?ep=10.0.0.5:8790&code=ABCD2345'));
    fixture.detectChanges();
    await settle();
    expect(el().querySelector('[data-testid="pair-confirm"]')).not.toBeNull();
    jasmine.clock().install();
    jasmine.clock().mockDate(new Date());
    try {
      el().querySelector<HTMLButtonElement>('[data-testid="pair-confirm"]')!.click();
      await settleFaked();
      expect(el().querySelector('[data-testid="pair-view"]')?.getAttribute('data-phase')).toBe('waiting');
      jasmine.clock().tick(61_000);
      fixture.detectChanges();
      expect(el().querySelector('[data-testid="pair-elapsed"]')?.textContent).toContain('1:0');
    } finally {
      jasmine.clock().uninstall();
    }
  });

  const failures: [PairErrorCode, number, string][] = [
    ['pair_rejected', 403, 'rejected'],
    ['pair_invalid_code', 404, 'Unknown pairing code'],
    ['pair_already_requested', 409, 'Another device'],
    ['pair_expired', 410, 'expired'],
    ['pair_locked', 429, 'locked'],
    ['pair_timeout', 408, '90 seconds'],
  ];
  for (const [code, status, phrase] of failures) {
    it(`${status} ${code} -> its own message ("${phrase}")`, async () => {
      pairSpy.and.rejectWith(new PairError(code, status));
      fixture.componentRef.setInput('invite', parsePairUrl('bebok://pair?ep=10.0.0.5:8790&code=ABCD2345'));
      fixture.detectChanges();
      await settle();
      el().querySelector<HTMLButtonElement>('[data-testid="pair-confirm"]')!.click();
      await settle();
      const error = el().querySelector('[data-testid="pair-error"]');
      expect(error?.getAttribute('data-code')).toBe(code);
      expect(error?.textContent).toContain(phrase);
      expect(switchSpy).not.toHaveBeenCalled();
      expect(!!el().querySelector('[data-testid="pair-retry"]')).toBe(code === 'pair_timeout');
    });
  }

  it('the messages of the six outcomes are pairwise distinct', () => {
    const texts = failures.map(([code]) =>
      fixture.componentInstance.failureMessage({ code, detail: '', retryable: false }),
    );
    expect(new Set(texts).size).toBe(texts.length);
  });

  it('no route offers the Tailscale shortcut', async () => {
    flow.probeFetch = jasmine.createSpy('fetch').and.rejectWith(new TypeError('Failed to fetch'));
    fixture.componentRef.setInput('invite', parsePairUrl('bebok://pair?ep=100.64.0.7:8790&code=ABCD2345'));
    fixture.detectChanges();
    await settle();
    expect(el().querySelector('[data-testid="pair-error"]')?.getAttribute('data-code')).toBe('probe_no_route');
    expect(el().querySelector('[data-testid="pair-open-tailscale"]')).not.toBeNull();
  });
});
