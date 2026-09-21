/**
 * F9-17: `?engine=<url>` bootstrap. A launcher opens the browser client at
 * `http://localhost:4200/?engine=<encoded BEBOK_READY url>`; the transport
 * adopts that address (URL + capability token) once and strips the parameter
 * from the address bar so the token is not kept in history/bookmarks.
 */

import { getEngineToken, setEngineToken } from './auth.interceptor';
import {
  BOOTSTRAP_ENGINE_PARAM,
  TransportStrategy,
  readBootstrapEngine,
  stripBootstrapParam,
} from './transport.strategy';

describe('readBootstrapEngine', () => {
  it('returns the decoded engine URL including its token', () => {
    const engine = 'http://127.0.0.1:8812/?token=abc123';
    const search = `?${BOOTSTRAP_ENGINE_PARAM}=${encodeURIComponent(engine)}`;
    expect(readBootstrapEngine(search)).toBe(engine);
  });

  it('accepts a plain (no-auth) engine URL', () => {
    expect(readBootstrapEngine('?engine=http%3A%2F%2Flocalhost%3A8787')).toBe(
      'http://localhost:8787',
    );
  });

  it('returns null when the parameter is missing, empty or not http(s)', () => {
    expect(readBootstrapEngine('')).toBeNull();
    expect(readBootstrapEngine('?foo=bar')).toBeNull();
    expect(readBootstrapEngine('?engine=')).toBeNull();
    expect(readBootstrapEngine('?engine=%20')).toBeNull();
    expect(readBootstrapEngine('?engine=not-a-url')).toBeNull();
    expect(readBootstrapEngine('?engine=javascript%3Aalert(1)')).toBeNull();
    expect(readBootstrapEngine('?engine=file%3A%2F%2F%2Fetc%2Fpasswd')).toBeNull();
  });
});

describe('stripBootstrapParam', () => {
  it('removes only the engine parameter, keeping other params and the hash', () => {
    const href = 'http://localhost:4200/?engine=http%3A%2F%2Fx%3A1%2F%3Ftoken%3Dt&lang=pl#frag';
    expect(stripBootstrapParam(href)).toBe('http://localhost:4200/?lang=pl#frag');
  });

  it('drops the empty "?" when engine was the only parameter', () => {
    expect(stripBootstrapParam('http://localhost:4200/?engine=http%3A%2F%2Fx%3A1')).toBe(
      'http://localhost:4200/',
    );
  });
});

describe('TransportStrategy bootstrap adoption', () => {
  const originalHref = window.location.href;

  beforeEach(() => {
    localStorage.clear();
    setEngineToken(null);
  });

  afterEach(() => {
    window.history.replaceState(null, '', originalHref);
    localStorage.clear();
    setEngineToken(null);
  });

  function loadPageWith(search: string): void {
    const url = new URL(originalHref);
    url.search = search;
    window.history.replaceState(null, '', url.toString());
  }

  it('persists URL + token from ?engine= and strips it from the address bar', async () => {
    const engine = 'http://127.0.0.1:8812/?token=tok-xyz';
    loadPageWith(`?${BOOTSTRAP_ENGINE_PARAM}=${encodeURIComponent(engine)}`);

    const transport = new TransportStrategy();

    expect(transport.readRemoteUrl()).toBe('http://127.0.0.1:8812');
    expect(transport.readRemoteToken()).toBe('tok-xyz');
    expect(new URL(window.location.href).searchParams.has(BOOTSTRAP_ENGINE_PARAM)).toBeFalse();

    // The regular connect() path (browser mode) now resolves to that engine
    // and installs the token for authFetch - no manual paste needed.
    const conn = await transport.connect();
    expect(conn).toEqual({ kind: 'http', baseUrl: 'http://127.0.0.1:8812' });
    expect(getEngineToken()).toBe('tok-xyz');
  });

  it('a no-auth bootstrap URL replaces a stale token from an earlier engine', () => {
    localStorage.setItem('bebok.remote.baseUrl', 'http://127.0.0.1:9999');
    localStorage.setItem('bebok.remote.token', 'stale');
    loadPageWith(`?${BOOTSTRAP_ENGINE_PARAM}=${encodeURIComponent('http://localhost:8812/')}`);

    const transport = new TransportStrategy();

    expect(transport.readRemoteUrl()).toBe('http://localhost:8812');
    expect(transport.readRemoteToken()).toBeNull();
  });

  it('leaves stored settings and the URL alone when no ?engine= is present', () => {
    localStorage.setItem('bebok.remote.baseUrl', 'http://127.0.0.1:9999');
    localStorage.setItem('bebok.remote.token', 'keep');
    loadPageWith('?lang=pl');

    const transport = new TransportStrategy();

    expect(transport.readRemoteUrl()).toBe('http://127.0.0.1:9999');
    expect(transport.readRemoteToken()).toBe('keep');
    expect(window.location.search).toBe('?lang=pl');
  });

  it('ignores an invalid ?engine= value', () => {
    localStorage.setItem('bebok.remote.baseUrl', 'http://127.0.0.1:9999');
    loadPageWith(`?${BOOTSTRAP_ENGINE_PARAM}=nonsense`);

    const transport = new TransportStrategy();

    expect(transport.readRemoteUrl()).toBe('http://127.0.0.1:9999');
  });
});


describe('TransportStrategy fixed profile (Phase 2)', () => {
  const originalHref = window.location.href;

  beforeEach(() => {
    localStorage.clear();
    setEngineToken(null);
  });

  afterEach(() => {
    window.history.replaceState(null, '', originalHref);
    localStorage.clear();
    setEngineToken(null);
  });

  it('readProfile returns manual by default', () => {
    const transport = new TransportStrategy();
    const profile = transport.readProfile();
    expect(profile.kind).toBe('manual');
    expect(profile.baseUrl).toBe('http://127.0.0.1:8787');
    expect(profile.token).toBeNull();
  });

  it('saveFixedProfile persists URL + token + kind=fixed', () => {
    const transport = new TransportStrategy();
    transport.saveFixedProfile('http://127.0.0.1:9999', 'tok-abc');
    expect(transport.readRemoteUrl()).toBe('http://127.0.0.1:9999');
    expect(transport.readRemoteToken()).toBe('tok-abc');
    expect(transport.isFixedProfile()).toBeTrue();
    expect(transport.readProfile().kind).toBe('fixed');
  });

  it('clearProfile reverts to manual and removes stored values', () => {
    const transport = new TransportStrategy();
    transport.saveFixedProfile('http://127.0.0.1:9999', 'tok-abc');
    expect(transport.isFixedProfile()).toBeTrue();
    transport.clearProfile();
    expect(transport.isFixedProfile()).toBeFalse();
    expect(transport.readProfile().kind).toBe('manual');
    expect(transport.readProfile().baseUrl).toBe('http://127.0.0.1:8787');
    expect(transport.readRemoteToken()).toBeNull();
  });

  it('connect() uses saved fixed profile URL', async () => {
    const transport = new TransportStrategy();
    transport.saveFixedProfile('http://127.0.0.1:9999', 'tok-abc');
    // Construct a new transport to simulate a page reload with saved profile.
    const transport2 = new TransportStrategy();
    const conn = await transport2.connect();
    expect(conn.baseUrl).toBe('http://127.0.0.1:9999');
    expect(getEngineToken()).toBe('tok-abc');
  });
});
