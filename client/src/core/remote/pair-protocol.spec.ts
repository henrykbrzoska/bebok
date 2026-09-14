/**
 * WP-M6 (F10-22/F10-23): `bebok://pair` URL parsing and the `POST /remote/pair`
 * status-code mapping.
 */

import {
  PAIR_TIMEOUT_MS,
  PairError,
  PairUrlError,
  buildPairUrl,
  normalizeEndpoint,
  normalizePairCode,
  pairWithDesktop,
  parsePairUrl,
} from './pair-protocol';

describe('parsePairUrl (WP-M6 / F10-22)', () => {
  it('parses the WP-M4 QR format with every key', () => {
    const invite = parsePairUrl(
      'bebok://pair?v=1&ep=http://100.64.0.7:8790,192.168.1.20:8790&code=abcd2345&fp=fp-1',
    );
    expect(invite).toEqual({
      version: 1,
      endpoints: ['http://100.64.0.7:8790', 'http://192.168.1.20:8790'],
      code: 'ABCD2345',
      fingerprint: 'fp-1',
    });
  });

  it('dedupes endpoints and tolerates a missing fingerprint', () => {
    const invite = parsePairUrl('bebok://pair?ep=1.2.3.4:1,http://1.2.3.4:1/&code=X');
    expect(invite.endpoints).toEqual(['http://1.2.3.4:1']);
    expect(invite.fingerprint).toBeNull();
    expect(invite.version).toBe(1);
  });

  it('round-trips through buildPairUrl', () => {
    const url = buildPairUrl({ endpoints: ['http://10.0.0.2:8790'], code: 'CODE2345', fingerprint: 'f' });
    expect(url.startsWith('bebok://pair?')).toBeTrue();
    expect(parsePairUrl(url)).toEqual({
      version: 1,
      endpoints: ['http://10.0.0.2:8790'],
      code: 'CODE2345',
      fingerprint: 'f',
    });
  });

  const malformed: [string, PairUrlError['reason']][] = [
    ['not a url at all', 'not_pair_url'],
    ['https://example.com/pair?code=X&ep=1.2.3.4', 'not_pair_url'],
    ['bebok://other?code=X&ep=1.2.3.4', 'not_pair_url'],
    ['bebok://pair?v=2&code=X&ep=1.2.3.4', 'unsupported_version'],
    ['bebok://pair?v=1&ep=1.2.3.4', 'missing_code'],
    ['bebok://pair?v=1&code=X', 'missing_endpoints'],
    ['bebok://pair?v=1&code=X&ep=,', 'missing_endpoints'],
    ['bebok://pair?v=1&code=X&ep=ftp://1.2.3.4', 'bad_endpoint'],
  ];
  for (const [url, reason] of malformed) {
    it(`rejects "${url}" as ${reason}`, () => {
      expect(() => parsePairUrl(url)).toThrowMatching(
        (err) => err instanceof PairUrlError && err.reason === reason,
      );
    });
  }
});

describe('normalizeEndpoint / normalizePairCode', () => {
  it('adds the scheme, keeps the path (relay tunnels), drops query and trailing slash', () => {
    expect(normalizeEndpoint('100.64.0.7:8790')).toBe('http://100.64.0.7:8790');
    expect(normalizeEndpoint('https://desk.tail.ts.net:8790/?y')).toBe('https://desk.tail.ts.net:8790');
    expect(normalizeEndpoint('https://relay.example.workers.dev/t/0123456789abcdef0123456789abcdef/')).toBe(
      'https://relay.example.workers.dev/t/0123456789abcdef0123456789abcdef',
    );
    expect(normalizeEndpoint('  ')).toBeNull();
    expect(normalizeEndpoint('ws://1.2.3.4')).toBeNull();
  });

  it('upper-cases and removes separators from a typed code', () => {
    expect(normalizePairCode(' abcd-2345 ')).toBe('ABCD2345');
  });
});

describe('pairWithDesktop (WP-M6 / F10-23)', () => {
  function fetchReturning(status: number, body: unknown): jasmine.Spy {
    return jasmine
      .createSpy('fetch')
      .and.callFake(async () => new Response(JSON.stringify(body), { status }));
  }

  it('POSTs the code, device name and metadata to <endpoint>/remote/pair', async () => {
    const fetchSpy = fetchReturning(200, {
      deviceId: 'd1',
      token: 't1',
      engineName: 'desk',
      fingerprint: 'fp',
    });
    const result = await pairWithDesktop('100.64.0.7:8790', 'abcd2345', 'Pixel', {
      fetch: fetchSpy,
      model: 'SM-S938B',
      platform: 'android',
    });
    expect(result).toEqual({ deviceId: 'd1', token: 't1', engineName: 'desk', fingerprint: 'fp' });
    const [url, init] = fetchSpy.calls.mostRecent().args as [string, RequestInit];
    expect(url).toBe('http://100.64.0.7:8790/remote/pair');
    expect(init.method).toBe('POST');
    expect(JSON.parse(init.body as string)).toEqual({
      code: 'ABCD2345',
      deviceName: 'Pixel',
      model: 'SM-S938B',
      platform: 'android',
    });
  });

  const codes: [number, string, PairError['code'], boolean][] = [
    [403, 'pair_rejected', 'pair_rejected', false],
    [404, 'pair_invalid_code', 'pair_invalid_code', false],
    [409, 'pair_already_requested', 'pair_already_requested', false],
    [410, 'pair_expired', 'pair_expired', false],
    [429, 'pair_locked', 'pair_locked', false],
    [408, 'pair_timeout', 'pair_timeout', true],
  ];
  for (const [status, engineCode, expected, retryable] of codes) {
    it(`maps ${status} ${engineCode} to PairError.${expected}`, async () => {
      const fetchSpy = fetchReturning(status, { error: engineCode, message: 'x' });
      let caught: unknown;
      try {
        await pairWithDesktop('http://1.2.3.4:1', 'ABCD2345', 'p', { fetch: fetchSpy });
      } catch (err) {
        caught = err;
      }
      expect(caught).toBeInstanceOf(PairError);
      const err = caught as PairError;
      expect(err.code).toBe(expected);
      expect(err.status).toBe(status);
      expect(err.retryable).toBe(retryable);
    });
  }

  it('maps a status without a body to the code of the status', async () => {
    const fetchSpy = jasmine
      .createSpy('fetch')
      .and.callFake(async () => new Response('', { status: 410 }));
    await expectAsync(
      pairWithDesktop('http://1.2.3.4:1', 'ABCD2345', 'p', { fetch: fetchSpy }),
    ).toBeRejectedWithError(PairError, /pair_expired/);
  });

  it('maps a network failure to `unreachable` (retryable)', async () => {
    const fetchSpy = jasmine.createSpy('fetch').and.rejectWith(new TypeError('Failed to fetch'));
    let caught: unknown;
    try {
      await pairWithDesktop('http://1.2.3.4:1', 'ABCD2345', 'p', { fetch: fetchSpy });
    } catch (err) {
      caught = err;
    }
    expect((caught as PairError).code).toBe('unreachable');
    expect((caught as PairError).retryable).toBeTrue();
  });

  it('gives up after the client timeout (>= 95 s by default)', async () => {
    jasmine.clock().install();
    try {
      expect(PAIR_TIMEOUT_MS).toBeGreaterThanOrEqual(95_000);
      const fetchSpy = jasmine.createSpy('fetch').and.callFake(
        (_url: string, init: RequestInit) =>
          new Promise<Response>((_resolve, reject) => {
            (init.signal as AbortSignal).addEventListener('abort', () =>
              reject(new DOMException('aborted', 'AbortError')),
            );
          }),
      );
      const pending = pairWithDesktop('http://1.2.3.4:1', 'ABCD2345', 'p', {
        fetch: fetchSpy,
        timeoutMs: 1000,
      });
      const outcome = pending.then(
        () => 'resolved',
        (err: PairError) => err.code,
      );
      jasmine.clock().tick(1001);
      await expectAsync(outcome).toBeResolvedTo('unreachable');
    } finally {
      jasmine.clock().uninstall();
    }
  });
});
