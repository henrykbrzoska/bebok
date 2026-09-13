/**
 * WP-M6 (F10-23): the pairing flow against a mocked `EngineApi` - every
 * `POST /remote/pair` outcome maps to a distinct failure code and
 * `switchTarget` fires exactly once, only on success.
 */

import { TestBed } from '@angular/core/testing';
import { provideZonelessChangeDetection, signal } from '@angular/core';

import { ENGINE_API, EngineApi } from '../engine-api';
import { EngineTargetStore } from '../engine-target.store';
import { PairError, PairErrorCode } from './pair-protocol';
import { PairingFlow, desktopTargetId } from './pairing';

function okStatusFetch(): jasmine.Spy {
  return jasmine.createSpy('fetch').and.callFake(
    async () =>
      new Response(JSON.stringify({ engineName: 'rafal-pc', fingerprint: 'fp-1' }), { status: 200 }),
  );
}

describe('PairingFlow (WP-M6 / F10-23)', () => {
  let flow: PairingFlow;
  let targets: EngineTargetStore;
  let pairSpy: jasmine.Spy;
  let switchSpy: jasmine.Spy;

  beforeEach(() => {
    localStorage.clear();
    pairSpy = jasmine.createSpy('pairWithDesktop');
    switchSpy = jasmine.createSpy('switchTarget').and.callFake(async (id: string) => {
      const target = TestBed.inject(EngineTargetStore).byId(id)!;
      TestBed.inject(EngineTargetStore).setActive(id);
      return target;
    });
    const engine = {
      connection: signal(null),
      unauthorized: signal(false),
      pairWithDesktop: pairSpy,
      switchTarget: switchSpy,
    } as unknown as EngineApi;
    TestBed.configureTestingModule({
      providers: [provideZonelessChangeDetection(), { provide: ENGINE_API, useValue: engine }],
    });
    flow = TestBed.inject(PairingFlow);
    targets = TestBed.inject(EngineTargetStore);
    flow.probeFetch = okStatusFetch();
    flow.probeTimeoutMs = 50;
  });

  afterEach(() => localStorage.clear());

  const invite = { endpoints: ['http://100.64.0.7:8790'], code: 'abcd2345', fingerprint: 'fp-1' };

  it('probe -> confirm exposes the engine name and the normalised code', async () => {
    expect(await flow.probe(invite)).toBeTrue();
    expect(flow.phase()).toBe('confirm');
    expect(flow.candidate()).toEqual(
      jasmine.objectContaining({
        endpoint: 'http://100.64.0.7:8790',
        engineName: 'rafal-pc',
        code: 'ABCD2345',
      }),
    );
  });

  it('refuses a public http endpoint before touching the network (F10-28)', async () => {
    expect(await flow.probe({ ...invite, endpoints: ['http://8.8.8.8:8790'] })).toBeFalse();
    expect(flow.failure()?.code).toBe('endpoint_not_private');
    expect(flow.probeFetch).not.toHaveBeenCalled();
  });

  it('probe failure lands in error with probe_no_route', async () => {
    flow.probeFetch = jasmine.createSpy('fetch').and.rejectWith(new TypeError('Failed to fetch'));
    expect(await flow.probe(invite)).toBeFalse();
    expect(flow.phase()).toBe('error');
    expect(flow.failure()?.code).toBe('probe_no_route');
    expect(flow.failure()?.retryable).toBeTrue();
  });

  it('200 -> upsert desktop target + switchTarget once -> paired', async () => {
    pairSpy.and.resolveTo({ deviceId: 'dev-1', token: 'tok-1', engineName: 'rafal-pc', fingerprint: 'fp-1' });
    await flow.probe(invite);
    expect(await flow.pair('My phone')).toBeTrue();
    expect(flow.phase()).toBe('paired');
    expect(pairSpy).toHaveBeenCalledWith(
      'http://100.64.0.7:8790',
      'ABCD2345',
      'My phone',
      jasmine.objectContaining({ platform: 'android' }),
    );
    const id = desktopTargetId('dev-1');
    expect(flow.pairedTargetId()).toBe(id);
    expect(targets.byId(id)).toEqual(
      jasmine.objectContaining({
        kind: 'desktop',
        label: 'rafal-pc',
        baseUrl: 'http://100.64.0.7:8790',
        token: 'tok-1',
      }),
    );
    expect(switchSpy).toHaveBeenCalledTimes(1);
    expect(switchSpy).toHaveBeenCalledWith(id);
    expect(targets.activeId()).toBe(id);
    // Persisted for the next launch (token via TargetSecrets).
    expect(JSON.parse(localStorage.getItem('bebok.targets')!)).toEqual([
      jasmine.objectContaining({ id, kind: 'desktop' }),
    ]);
  });

  const failures: [PairErrorCode, number, boolean][] = [
    ['pair_rejected', 403, false],
    ['pair_invalid_code', 404, false],
    ['pair_already_requested', 409, false],
    ['pair_expired', 410, false],
    ['pair_locked', 429, false],
    ['pair_timeout', 408, true],
  ];
  for (const [code, status, retryable] of failures) {
    it(`${status} ${code} -> error.${code}, no target, no switchTarget`, async () => {
      pairSpy.and.rejectWith(new PairError(code, status, 'x'));
      await flow.probe(invite);
      expect(await flow.pair('p')).toBeFalse();
      expect(flow.phase()).toBe('error');
      expect(flow.failure()).toEqual(jasmine.objectContaining({ code, retryable }));
      expect(switchSpy).not.toHaveBeenCalled();
      expect(targets.targets().filter((t) => t.kind === 'desktop')).toEqual([]);
    });
  }

  it('a retryable failure goes back to confirm with the same code (408 path)', async () => {
    pairSpy.and.rejectWith(new PairError('pair_timeout', 408));
    await flow.probe(invite);
    await flow.pair('p');
    flow.retry();
    expect(flow.phase()).toBe('confirm');
    expect(flow.candidate()?.code).toBe('ABCD2345');
    expect(flow.failure()).toBeNull();
  });

  it('a non-retryable failure cannot be retried', async () => {
    pairSpy.and.rejectWith(new PairError('pair_expired', 410));
    await flow.probe(invite);
    await flow.pair('p');
    flow.retry();
    expect(flow.phase()).toBe('error');
  });

  it('cancel during the long-poll returns to confirm without switching', async () => {
    let abortSignal: AbortSignal | null = null;
    pairSpy.and.callFake(
      (_e: string, _c: string, _n: string, extra: { signal: AbortSignal }) =>
        new Promise((_resolve, reject) => {
          abortSignal = extra.signal;
          extra.signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
        }),
    );
    await flow.probe(invite);
    const pending = flow.pair('p');
    expect(flow.phase()).toBe('waiting');
    expect(flow.waitingSince()).not.toBeNull();
    flow.cancel();
    expect(await pending).toBeFalse();
    expect(abortSignal!.aborted).toBeTrue();
    expect(flow.phase()).toBe('confirm');
    expect(switchSpy).not.toHaveBeenCalled();
  });
});
