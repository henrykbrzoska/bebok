/**
 * The engine fetch wrapper: attaches the capability token and, when the
 * engine answers 401 (token from a previous engine launch), notifies the
 * unauthorized listeners exactly once per response.
 */

import { authFetch, onEngineUnauthorized, setEngineToken, splitEngineUrl } from './auth.interceptor';

describe('authFetch', () => {
  let originalFetch: typeof fetch;
  let lastInit: RequestInit | undefined;
  let status = 200;

  beforeEach(() => {
    originalFetch = window.fetch;
    status = 200;
    window.fetch = ((_url: RequestInfo | URL, init?: RequestInit) => {
      lastInit = init;
      return Promise.resolve(new Response('{}', { status }));
    }) as typeof fetch;
  });

  afterEach(() => {
    window.fetch = originalFetch;
    setEngineToken(null);
  });

  it('attaches the engine token as a bearer header', async () => {
    setEngineToken('tok-1');
    await authFetch('http://engine/session');
    expect(new Headers(lastInit?.headers).get('Authorization')).toBe('Bearer tok-1');
  });

  it('notifies unauthorized listeners on a 401 and not otherwise', async () => {
    const seen: number[] = [];
    const off = onEngineUnauthorized(() => seen.push(1));
    try {
      await authFetch('http://engine/stats');
      expect(seen.length).toBe(0);

      status = 401;
      const res = await authFetch('http://engine/stats');
      expect(res.status).toBe(401);
      expect(seen.length).toBe(1);

      status = 500;
      await authFetch('http://engine/stats');
      expect(seen.length).toBe(1);
    } finally {
      off();
    }
  });

  it('splits a BEBOK_READY line into base URL and token', () => {
    expect(splitEngineUrl('http://127.0.0.1:9000/?token=abc')).toEqual({
      baseUrl: 'http://127.0.0.1:9000',
      token: 'abc',
    });
    expect(splitEngineUrl('http://127.0.0.1:9000/')).toEqual({
      baseUrl: 'http://127.0.0.1:9000',
      token: null,
    });
  });
});
