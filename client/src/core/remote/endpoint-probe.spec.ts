/**
 * WP-M6 (F10-22): the endpoint race and the F10-28 address classes.
 */

import {
  ProbeError,
  hostClass,
  isAllowedRemoteEndpoint,
  isAllowedRemoteHost,
  probeEndpoints,
} from './endpoint-probe';

type Behaviour =
  | { kind: 'ok'; afterMs: number; body?: unknown }
  | { kind: 'status'; afterMs: number; status: number; text?: string }
  | { kind: 'hang' }
  | { kind: 'refuse'; afterMs: number };

/** A `fetch` whose behaviour is scripted per endpoint and driven by the fake clock. */
function scriptedFetch(script: Record<string, Behaviour>): jasmine.Spy {
  return jasmine.createSpy('fetch').and.callFake((url: string, init: RequestInit = {}) => {
    const base = url.replace(/\/remote\/status$/, '');
    const behaviour = script[base] ?? { kind: 'hang' };
    const signal = init.signal as AbortSignal;
    return new Promise<Response>((resolve, reject) => {
      signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')));
      if (behaviour.kind === 'hang') {
        return;
      }
      setTimeout(() => {
        if (signal.aborted) {
          return;
        }
        if (behaviour.kind === 'ok') {
          resolve(
            new Response(JSON.stringify(behaviour.body ?? { engineName: 'desk', fingerprint: 'fp' }), {
              status: 200,
            }),
          );
        } else if (behaviour.kind === 'status') {
          resolve(new Response(behaviour.text ?? '{}', { status: behaviour.status }));
        } else {
          reject(new TypeError('Failed to fetch'));
        }
      }, behaviour.afterMs);
    });
  });
}

describe('probeEndpoints (WP-M6 / F10-22)', () => {
  beforeEach(() => jasmine.clock().install());
  afterEach(() => jasmine.clock().uninstall());

  it('resolves with the first 200 and aborts the rest', async () => {
    const fetchSpy = scriptedFetch({
      'http://100.64.0.7:8790': { kind: 'hang' },
      'http://192.168.1.20:8790': { kind: 'ok', afterMs: 40 },
    });
    let now = 0;
    const pending = probeEndpoints(['100.64.0.7:8790', '192.168.1.20:8790'], {
      fetch: fetchSpy,
      timeoutMs: 3000,
      now: () => now,
    });
    now = 40;
    jasmine.clock().tick(40);
    const outcome = await pending;
    expect(outcome.endpoint).toBe('http://192.168.1.20:8790');
    expect(outcome.status.engineName).toBe('desk');
    expect(outcome.elapsedMs).toBe(40);
    // Both candidates were tried in parallel; the tailnet one was aborted.
    expect(fetchSpy).toHaveBeenCalledTimes(2);
    const tailnetInit = fetchSpy.calls.argsFor(0)[1] as RequestInit;
    expect((tailnetInit.signal as AbortSignal).aborted).toBeTrue();
  });

  it('finishes a LAN-only phone in one timeout (~3 s), not one per candidate', async () => {
    const fetchSpy = scriptedFetch({
      'http://100.64.0.7:8790': { kind: 'hang' },
      'http://192.168.1.20:8790': { kind: 'hang' },
    });
    const settledRef: { value: string | null } = { value: null };
    probeEndpoints(['http://100.64.0.7:8790', 'http://192.168.1.20:8790'], {
      fetch: fetchSpy,
      timeoutMs: 3000,
    }).then(
      () => (settledRef.value = 'ok'),
      (err: ProbeError) => (settledRef.value = err.kind),
    );
    jasmine.clock().tick(2999);
    await Promise.resolve();
    expect(settledRef.value).toBeNull();
    jasmine.clock().tick(2);
    for (let i = 0; i < 5; i++) {
      await Promise.resolve();
    }
    expect(settledRef.value).toBe('no_route');
  });

  it('classifies "every candidate refused" as no_route (Tailscale off)', async () => {
    const fetchSpy = scriptedFetch({
      'http://100.64.0.7:8790': { kind: 'refuse', afterMs: 5 },
      'http://192.168.1.20:8790': { kind: 'refuse', afterMs: 5 },
    });
    const pending = probeEndpoints(['http://100.64.0.7:8790', 'http://192.168.1.20:8790'], {
      fetch: fetchSpy,
    });
    jasmine.clock().tick(10);
    let caught: ProbeError | null = null;
    try {
      await pending;
    } catch (err) {
      caught = err as ProbeError;
    }
    expect(caught?.kind).toBe('no_route');
    expect(caught?.attempts.map((a) => a.failure)).toEqual(['network', 'network']);
  });

  it('classifies a non-200 answer as bad_endpoint', async () => {
    const fetchSpy = scriptedFetch({
      'http://100.64.0.7:8790': { kind: 'refuse', afterMs: 5 },
      'http://192.168.1.20:8790': { kind: 'status', afterMs: 5, status: 503 },
    });
    const pending = probeEndpoints(['http://100.64.0.7:8790', 'http://192.168.1.20:8790'], {
      fetch: fetchSpy,
    });
    jasmine.clock().tick(10);
    let caught: ProbeError | null = null;
    try {
      await pending;
    } catch (err) {
      caught = err as ProbeError;
    }
    expect(caught?.kind).toBe('bad_endpoint');
    expect(caught?.attempts.find((a) => a.endpoint === 'http://192.168.1.20:8790')?.status).toBe(503);
  });

  it('an unpaired probe treats the engine 401 as reachable-but-unauthenticated', async () => {
    const fetchSpy = scriptedFetch({
      'http://100.64.0.7:8790': { kind: 'hang' },
      'http://192.168.1.20:8790': {
        kind: 'status',
        afterMs: 5,
        status: 401,
        text: 'missing or invalid engine token',
      },
    });
    const pending = probeEndpoints(['http://100.64.0.7:8790', 'http://192.168.1.20:8790'], {
      fetch: fetchSpy,
    });
    jasmine.clock().tick(10);
    const outcome = await pending;
    expect(outcome.endpoint).toBe('http://192.168.1.20:8790');
    expect(outcome.authenticated).toBeFalse();
    expect(outcome.status).toEqual({});
  });

  it('a 401 from something other than the engine auth layer is bad_status', async () => {
    const fetchSpy = scriptedFetch({
      'http://192.168.1.20:8790': { kind: 'status', afterMs: 5, status: 401, text: 'Unauthorized' },
    });
    const pending = probeEndpoints(['http://192.168.1.20:8790'], { fetch: fetchSpy });
    jasmine.clock().tick(10);
    let caught: ProbeError | null = null;
    try {
      await pending;
    } catch (err) {
      caught = err as ProbeError;
    }
    expect(caught?.kind).toBe('bad_endpoint');
  });

  it('rejects a 200 from a different engine when a fingerprint is expected', async () => {
    const fetchSpy = scriptedFetch({
      'http://1.2.3.4:1': { kind: 'ok', afterMs: 1, body: { engineName: 'other', fingerprint: 'nope' } },
    });
    const pending = probeEndpoints(['http://1.2.3.4:1'], { fetch: fetchSpy, expectFingerprint: 'fp' });
    jasmine.clock().tick(5);
    let caught: ProbeError | null = null;
    try {
      await pending;
    } catch (err) {
      caught = err as ProbeError;
    }
    expect(caught?.kind).toBe('bad_endpoint');
    expect(caught?.attempts[0].failure).toBe('fingerprint');
  });

  it('rejects an empty candidate list', async () => {
    await expectAsync(probeEndpoints([], { fetch: jasmine.createSpy() })).toBeRejectedWithError(
      ProbeError,
      /no_endpoints/,
    );
  });
});

describe('hostClass / isAllowedRemoteHost (WP-M6 / F10-28)', () => {
  const cases: [string, ReturnType<typeof hostClass>][] = [
    ['127.0.0.1', 'loopback'],
    ['localhost', 'loopback'],
    ['[::1]', 'loopback'],
    ['10.0.2.2', 'private'],
    ['172.16.5.5', 'private'],
    ['172.32.0.1', 'public'],
    ['192.168.1.20', 'private'],
    ['100.64.0.7', 'tailnet'],
    ['100.127.255.254', 'tailnet'],
    ['100.128.0.1', 'public'],
    ['169.254.1.1', 'link-local'],
    ['[fd7a:115c:a1e0::1]', 'tailnet'],
    ['8.8.8.8', 'public'],
    ['example.com', 'public'],
    ['desk.tail1234.ts.net', 'local-name'],
    ['desk.local', 'local-name'],
    ['desk', 'local-name'],
    ['', 'invalid'],
  ];
  for (const [host, cls] of cases) {
    it(`${host || '(empty)'} -> ${cls}`, () => {
      expect(hostClass(host)).toBe(cls);
      expect(isAllowedRemoteHost(host)).toBe(cls !== 'public' && cls !== 'invalid');
    });
  }

  it('allows https to anything but http only to private/tailnet hosts', () => {
    expect(isAllowedRemoteEndpoint('https://example.com:8790')).toBeTrue();
    expect(isAllowedRemoteEndpoint('http://example.com:8790')).toBeFalse();
    expect(isAllowedRemoteEndpoint('http://100.64.0.7:8790')).toBeTrue();
    expect(isAllowedRemoteEndpoint('http://8.8.8.8:80')).toBeFalse();
    expect(isAllowedRemoteEndpoint('garbage')).toBeFalse();
  });
});
