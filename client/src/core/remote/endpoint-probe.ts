/**
 * Endpoint probe (WP-M6 / F10-22): which of the candidate addresses in a
 * pairing QR actually reaches the desktop engine?
 *
 * The QR carries every address the remote listener bound (Tailscale
 * `100.64.0.0/10` first, then LAN when `allow_lan` is on). The phone may be
 * on the tailnet, on the same Wi-Fi, or both, so all candidates are raced:
 * `GET /remote/status` on each with a short per-request timeout, the first
 * `200` wins and the rest are aborted. A LAN-only phone therefore finishes in
 * one timeout (~3 s) instead of waiting for the tailnet address to die.
 *
 * Total failure is classified: every candidate failing at the network level
 * (no route / DNS / refused / timeout) means "Tailscale is off or the desktop
 * is unreachable" (`no_route`, F10-27's Tailscale-off state); an endpoint that
 * answered but not with a usable status is `bad_endpoint` (wrong engine,
 * proxy, remote disabled).
 *
 * F10-28 address classes: `hostClass()` / `isAllowedRemoteHost()` is the
 * client-side gate that replaces what Android's network-security config
 * cannot express (no CIDR entries) - the app only ever talks cleartext HTTP
 * to loopback, RFC1918, link-local and the Tailscale CGNAT range, plus
 * `.local`/`.ts.net`/single-label names that resolve inside those.
 */

import { normalizeEndpoint } from './pair-protocol';

/** `GET /remote/status` fields the probe reads (WP-M1 payload subset). */
export interface ProbeStatus {
  enabled?: boolean;
  listening?: boolean;
  engineName?: string;
  fingerprint?: string;
  endpoints?: string[];
  port?: number;
  allowLan?: boolean;
}

export type ProbeFailure = 'network' | 'timeout' | 'bad_status' | 'bad_body' | 'fingerprint';

export interface ProbeAttempt {
  endpoint: string;
  failure: ProbeFailure;
  status?: number;
}

export interface ProbeOutcome {
  /** Winning clean base URL. */
  endpoint: string;
  status: ProbeStatus;
  /** Milliseconds until the winner answered. */
  elapsedMs: number;
  /**
   * False when the engine answered 401: it is reachable and it is a Bebok
   * engine (the token check runs before every route on the remote listener
   * except `POST /remote/pair`), but without a device token there is no
   * status payload yet - `engineName` arrives with the pairing result.
   */
  authenticated: boolean;
}

export type ProbeErrorKind = 'no_route' | 'bad_endpoint' | 'no_endpoints';

export class ProbeError extends Error {
  constructor(
    readonly kind: ProbeErrorKind,
    readonly attempts: ProbeAttempt[],
  ) {
    super(
      `${kind}: ${attempts.map((a) => `${a.endpoint} ${a.failure}${a.status ? ` ${a.status}` : ''}`).join(', ') || 'no candidates'}`,
    );
    this.name = 'ProbeError';
  }
}

export const PROBE_TIMEOUT_MS = 3000;

export interface ProbeOptions {
  timeoutMs?: number;
  fetch?: typeof fetch;
  /** When set, a status whose `fingerprint` differs is treated as a failure. */
  expectFingerprint?: string | null;
  /** Bearer token to send (probing an already paired target). */
  token?: string | null;
  signal?: AbortSignal;
  now?: () => number;
}

/**
 * Race `GET /remote/status` across `endpoints`; resolve with the first 200,
 * reject with `ProbeError` once every candidate failed.
 */
export function probeEndpoints(
  endpoints: string[],
  options: ProbeOptions = {},
): Promise<ProbeOutcome> {
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  const timeoutMs = options.timeoutMs ?? PROBE_TIMEOUT_MS;
  const now = options.now ?? (() => Date.now());
  const candidates: string[] = [];
  for (const raw of endpoints) {
    const clean = normalizeEndpoint(raw);
    if (clean && !candidates.includes(clean)) {
      candidates.push(clean);
    }
  }
  if (candidates.length === 0) {
    return Promise.reject(new ProbeError('no_endpoints', []));
  }

  return new Promise<ProbeOutcome>((resolve, reject) => {
    const started = now();
    const controllers: AbortController[] = [];
    const attempts: ProbeAttempt[] = [];
    let settled = false;
    let pending = candidates.length;

    const abortAll = (): void => {
      for (const c of controllers) {
        c.abort();
      }
    };
    const onOuterAbort = (): void => {
      if (!settled) {
        settled = true;
        abortAll();
        reject(new ProbeError('no_route', attempts));
      }
    };
    options.signal?.addEventListener('abort', onOuterAbort, { once: true });

    const finishFailure = (attempt: ProbeAttempt): void => {
      attempts.push(attempt);
      pending -= 1;
      if (pending === 0 && !settled) {
        settled = true;
        options.signal?.removeEventListener('abort', onOuterAbort);
        const allNetwork = attempts.every(
          (a) => a.failure === 'network' || a.failure === 'timeout',
        );
        reject(new ProbeError(allNetwork ? 'no_route' : 'bad_endpoint', attempts));
      }
    };

    for (const endpoint of candidates) {
      const controller = new AbortController();
      controllers.push(controller);
      let timedOut = false;
      const timer = setTimeout(() => {
        timedOut = true;
        controller.abort();
      }, timeoutMs);
      const headers: Record<string, string> = { Accept: 'application/json' };
      if (options.token) {
        headers['Authorization'] = `Bearer ${options.token}`;
      }
      void (async () => {
        try {
          const res = await doFetch(`${endpoint}/remote/status`, {
            method: 'GET',
            headers,
            signal: controller.signal,
            cache: 'no-store',
          });
          clearTimeout(timer);
          if (settled) {
            return;
          }
          if (res.status === 401 && !options.token) {
            // Unpaired probe: a Bebok engine refuses everything but
            // `/remote/pair` without a token - that refusal *is* the
            // reachability signal. Confirm it came from the engine's auth
            // layer (its body names the token), then win the race with it.
            let text = '';
            try {
              text = await res.text();
            } catch {
              /* body optional */
            }
            if (!/token/i.test(text)) {
              finishFailure({ endpoint, failure: 'bad_status', status: res.status });
              return;
            }
            if (settled) {
              return;
            }
            settled = true;
            options.signal?.removeEventListener('abort', onOuterAbort);
            abortAll();
            resolve({ endpoint, status: {}, elapsedMs: now() - started, authenticated: false });
            return;
          }
          if (!res.ok) {
            finishFailure({ endpoint, failure: 'bad_status', status: res.status });
            return;
          }
          let status: ProbeStatus;
          try {
            status = (await res.json()) as ProbeStatus;
          } catch {
            finishFailure({ endpoint, failure: 'bad_body', status: res.status });
            return;
          }
          if (!status || typeof status !== 'object') {
            finishFailure({ endpoint, failure: 'bad_body', status: res.status });
            return;
          }
          if (
            options.expectFingerprint &&
            typeof status.fingerprint === 'string' &&
            status.fingerprint !== options.expectFingerprint
          ) {
            finishFailure({ endpoint, failure: 'fingerprint', status: res.status });
            return;
          }
          if (settled) {
            return;
          }
          settled = true;
          options.signal?.removeEventListener('abort', onOuterAbort);
          abortAll();
          resolve({ endpoint, status, elapsedMs: now() - started, authenticated: true });
        } catch {
          clearTimeout(timer);
          if (settled) {
            return;
          }
          finishFailure({ endpoint, failure: timedOut ? 'timeout' : 'network' });
        }
      })();
    }
  });
}

// ---------------------------------------------------------------------------
// F10-28: address classes
// ---------------------------------------------------------------------------

export type HostClass =
  'loopback' | 'private' | 'tailnet' | 'link-local' | 'local-name' | 'public' | 'invalid';

function parseIpv4(host: string): number[] | null {
  const parts = host.split('.');
  if (parts.length !== 4) {
    return null;
  }
  const octets: number[] = [];
  for (const p of parts) {
    if (!/^\d{1,3}$/.test(p)) {
      return null;
    }
    const n = Number(p);
    if (n > 255) {
      return null;
    }
    octets.push(n);
  }
  return octets;
}

/**
 * Classify a hostname / IP literal. IPv6 literals arrive bracketed from
 * `URL.hostname` (`[::1]`); only loopback and ULA/link-local ranges are
 * recognised there - the engine publishes IPv4 endpoints only in 1.6.
 */
export function hostClass(host: string): HostClass {
  const h = (host ?? '').trim().toLowerCase();
  if (!h) {
    return 'invalid';
  }
  if (h === 'localhost' || h.endsWith('.localhost')) {
    return 'loopback';
  }
  const v4 = parseIpv4(h);
  if (v4) {
    const [a, b] = v4;
    if (a === 127) {
      return 'loopback';
    }
    if (a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168)) {
      return 'private';
    }
    if (a === 100 && b >= 64 && b <= 127) {
      return 'tailnet';
    }
    if (a === 169 && b === 254) {
      return 'link-local';
    }
    return 'public';
  }
  if (h.startsWith('[') && h.endsWith(']')) {
    const v6 = h.slice(1, -1);
    if (v6 === '::1') {
      return 'loopback';
    }
    if (/^fe[89ab]/.test(v6)) {
      return 'link-local';
    }
    if (/^f[cd]/.test(v6)) {
      // ULA - Tailscale's `fd7a:115c:a1e0::/48` lives here.
      return 'tailnet';
    }
    return 'public';
  }
  if (
    h.endsWith('.local') ||
    h.endsWith('.ts.net') ||
    h.endsWith('.internal') ||
    !h.includes('.')
  ) {
    return 'local-name';
  }
  return 'public';
}

/**
 * The only hosts the phone will talk cleartext HTTP to (F10-28 decision B):
 * anything a public `http://` host is refused at pairing/manual entry, since
 * the Android NSC can no longer refuse it for us.
 */
export function isAllowedRemoteHost(host: string): boolean {
  const cls = hostClass(host);
  return cls !== 'public' && cls !== 'invalid';
}

/** `isAllowedRemoteHost` for a full base URL; `https://` is always fine. */
/** A `bebok-relay` tunnel endpoint (`https://<worker>/t/<32 hex>`). */
export function isRelayEndpoint(endpoint: string): boolean {
  try {
    return /^\/t\/[0-9a-f]{32}\/?$/.test(new URL(endpoint).pathname);
  } catch {
    return false;
  }
}

export function isAllowedRemoteEndpoint(endpoint: string): boolean {
  let url: URL;
  try {
    url = new URL(endpoint);
  } catch {
    return false;
  }
  if (url.protocol === 'https:') {
    return true;
  }
  if (url.protocol !== 'http:') {
    return false;
  }
  return isAllowedRemoteHost(url.hostname);
}
