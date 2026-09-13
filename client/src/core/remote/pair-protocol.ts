/**
 * Pairing protocol (WP-M6 / F10-22, F10-23): the wire-level half of pairing a
 * phone with a desktop engine - pure functions, no Angular.
 *
 * The desktop (WP-M4) shows a QR encoding
 *
 *     bebok://pair?v=1&ep=<endpoint,endpoint,…>&code=<code>&fp=<fingerprint>
 *
 * The phone parses it (`parsePairUrl`), races the endpoints
 * (`endpoint-probe.ts`) and then long-polls `POST /remote/pair` on the
 * winner (`pairWithDesktop`) until the desktop confirms or rejects. The pair
 * call is unauthenticated and only accepted on the engine's *remote*
 * listener (WP-M1), so it goes through plain `fetch`, not `authFetch`.
 *
 * `EngineClient.pairWithDesktop()` delegates here; keeping the logic in this
 * file means the pairing flow and its specs never depend on a live
 * `EngineConnection` (there is none yet while pairing).
 */

import type { PairWithDesktopResponse } from '../engine.dtos';

export const PAIR_URL_SCHEME = 'bebok:';
export const PAIR_URL_HOST = 'pair';
/** Only this QR version is understood. */
export const PAIR_URL_VERSION = 1;

/** Parsed `bebok://pair?…` invite. */
export interface PairInvite {
  version: number;
  /** Clean base URLs (`http://100.64.0.7:8790`), duplicates removed. */
  endpoints: string[];
  code: string;
  /** Engine install fingerprint from the QR, null when absent. */
  fingerprint: string | null;
}

export type PairUrlErrorReason =
  | 'not_pair_url'
  | 'unsupported_version'
  | 'missing_code'
  | 'missing_endpoints'
  | 'bad_endpoint';

export class PairUrlError extends Error {
  constructor(
    readonly reason: PairUrlErrorReason,
    detail?: string,
  ) {
    super(detail ? `${reason}: ${detail}` : reason);
    this.name = 'PairUrlError';
  }
}

/** Pairing codes: 8 chars from `ABCDEFGHJKLMNPQRSTUVWXYZ23456789` (WP-M1). */
export const PAIR_CODE_RE = /^[ABCDEFGHJKLMNPQRSTUVWXYZ23456789]{8}$/;

/**
 * Upper-case and strip the separators/whitespace a user may have typed
 * ("abcd-efgh" -> "ABCDEFGH"). No confusable-character mapping: the alphabet
 * has neither `0/O` nor `1/I`, so a typo stays a typo (the engine answers
 * 404 `pair_invalid_code` and the UI says so).
 */
export function normalizePairCode(raw: string): string {
  return (raw ?? '').toUpperCase().replace(/[\s-]+/g, '');
}

/**
 * Normalise an endpoint typed by hand or carried by the QR into a clean base
 * URL: `100.64.0.7:8790` -> `http://100.64.0.7:8790`; a trailing slash or
 * path is dropped. Returns null when it cannot be a URL at all.
 */
export function normalizeEndpoint(raw: string): string | null {
  const trimmed = (raw ?? '').trim();
  if (!trimmed) {
    return null;
  }
  const withScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(trimmed) ? trimmed : `http://${trimmed}`;
  let url: URL;
  try {
    url = new URL(withScheme);
  } catch {
    return null;
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') {
    return null;
  }
  if (!url.hostname) {
    return null;
  }
  return `${url.protocol}//${url.host}`;
}

/**
 * Parse a `bebok://pair?v=1&ep=…&code=…&fp=…` URL. Throws `PairUrlError`
 * with a precise reason (the UI maps each to a message).
 */
export function parsePairUrl(raw: string): PairInvite {
  const text = (raw ?? '').trim();
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    throw new PairUrlError('not_pair_url', 'not a URL');
  }
  // `bebok://pair?...` - some URL parsers treat unknown schemes as opaque
  // and leave the host empty, so accept either `host === 'pair'` or a
  // pathname starting with `//pair`.
  const host = url.host || url.pathname.replace(/^\/\//, '').split(/[/?#]/)[0];
  if (url.protocol !== PAIR_URL_SCHEME || host !== PAIR_URL_HOST) {
    throw new PairUrlError('not_pair_url', `${url.protocol}//${host}`);
  }
  const params = url.searchParams;
  const version = Number(params.get('v') ?? PAIR_URL_VERSION);
  if (!Number.isInteger(version) || version !== PAIR_URL_VERSION) {
    throw new PairUrlError('unsupported_version', String(params.get('v')));
  }
  const code = (params.get('code') ?? '').trim().toUpperCase();
  if (!code) {
    throw new PairUrlError('missing_code');
  }
  const rawEndpoints = (params.get('ep') ?? '')
    .split(',')
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
  if (rawEndpoints.length === 0) {
    throw new PairUrlError('missing_endpoints');
  }
  const endpoints: string[] = [];
  for (const entry of rawEndpoints) {
    const normalized = normalizeEndpoint(entry);
    if (!normalized) {
      throw new PairUrlError('bad_endpoint', entry);
    }
    if (!endpoints.includes(normalized)) {
      endpoints.push(normalized);
    }
  }
  const fingerprint = (params.get('fp') ?? '').trim() || null;
  return { version, endpoints, code, fingerprint };
}

/** `bebok://pair?…` for a manual/deep-link invite (also used by e2e). */
export function buildPairUrl(invite: Omit<PairInvite, 'version'> & { version?: number }): string {
  const params = new URLSearchParams();
  params.set('v', String(invite.version ?? PAIR_URL_VERSION));
  params.set('ep', invite.endpoints.join(','));
  params.set('code', invite.code);
  if (invite.fingerprint) {
    params.set('fp', invite.fingerprint);
  }
  return `${PAIR_URL_SCHEME}//${PAIR_URL_HOST}?${params.toString()}`;
}

// ---------------------------------------------------------------------------
// POST /remote/pair
// ---------------------------------------------------------------------------

/** `POST /remote/pair` -> 200 body (WP-M1); the DTO lives in `engine.dtos.ts`. */
export type PairResult = PairWithDesktopResponse;

/**
 * Every outcome of `POST /remote/pair` the UI distinguishes (WP-M1 codes
 * plus the two client-side ones).
 */
export type PairErrorCode =
  | 'pair_rejected'
  | 'pair_invalid_code'
  | 'pair_already_requested'
  | 'pair_expired'
  | 'pair_locked'
  | 'pair_timeout'
  | 'unreachable'
  | 'unknown';

const STATUS_TO_CODE: Record<number, PairErrorCode> = {
  403: 'pair_rejected',
  404: 'pair_invalid_code',
  409: 'pair_already_requested',
  410: 'pair_expired',
  429: 'pair_locked',
  408: 'pair_timeout',
};

export class PairError extends Error {
  constructor(
    readonly code: PairErrorCode,
    readonly status: number | null,
    detail?: string,
  ) {
    super(detail ? `${code} (${status ?? 'network'}): ${detail}` : `${code} (${status ?? 'network'})`);
    this.name = 'PairError';
  }

  /** True when the same code can be presented again (408: desktop was slow). */
  get retryable(): boolean {
    return this.code === 'pair_timeout' || this.code === 'unreachable';
  }
}

/** The engine long-polls up to 90 s; the client must wait longer than that. */
export const PAIR_TIMEOUT_MS = 95_000;

export interface PairOptions {
  model?: string;
  platform?: string;
  /** Overrides for tests. */
  fetch?: typeof fetch;
  timeoutMs?: number;
  signal?: AbortSignal;
}

/**
 * `POST /remote/pair` on `endpoint` (a clean base URL). Resolves with the
 * device token once the desktop confirmed; rejects with `PairError` mapping
 * each status code (see `PairErrorCode`). Network failures and the local
 * timeout map to `unreachable`.
 */
export async function pairWithDesktop(
  endpoint: string,
  code: string,
  deviceName: string,
  options: PairOptions = {},
): Promise<PairResult> {
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  const base = normalizeEndpoint(endpoint);
  if (!base) {
    throw new PairError('unreachable', null, `bad endpoint: ${endpoint}`);
  }
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), options.timeoutMs ?? PAIR_TIMEOUT_MS);
  const onOuterAbort = (): void => controller.abort();
  options.signal?.addEventListener('abort', onOuterAbort, { once: true });
  let res: Response;
  try {
    res = await doFetch(`${base}/remote/pair`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({
        code: code.trim().toUpperCase(),
        deviceName: deviceName.trim().slice(0, 64),
        ...(options.model ? { model: options.model } : {}),
        ...(options.platform ? { platform: options.platform } : {}),
      }),
      signal: controller.signal,
    });
  } catch (err) {
    throw new PairError('unreachable', null, err instanceof Error ? err.message : String(err));
  } finally {
    clearTimeout(timer);
    options.signal?.removeEventListener('abort', onOuterAbort);
  }
  if (res.ok) {
    const body = (await res.json()) as Partial<PairResult>;
    if (typeof body.deviceId !== 'string' || typeof body.token !== 'string') {
      throw new PairError('unknown', res.status, 'malformed pair response');
    }
    return {
      deviceId: body.deviceId,
      token: body.token,
      engineName: typeof body.engineName === 'string' ? body.engineName : '',
      fingerprint: typeof body.fingerprint === 'string' ? body.fingerprint : '',
    };
  }
  let detail = '';
  let engineCode: string | null = null;
  try {
    const text = await res.text();
    try {
      const parsed = JSON.parse(text) as { error?: unknown; message?: unknown };
      engineCode = typeof parsed.error === 'string' ? parsed.error : null;
      detail = typeof parsed.message === 'string' ? parsed.message : text;
    } catch {
      detail = text;
    }
  } catch {
    /* status only */
  }
  const code$ = isPairErrorCode(engineCode) ? engineCode : (STATUS_TO_CODE[res.status] ?? 'unknown');
  throw new PairError(code$, res.status, detail);
}

function isPairErrorCode(value: string | null): value is PairErrorCode {
  return (
    value === 'pair_rejected' ||
    value === 'pair_invalid_code' ||
    value === 'pair_already_requested' ||
    value === 'pair_expired' ||
    value === 'pair_locked' ||
    value === 'pair_timeout'
  );
}
