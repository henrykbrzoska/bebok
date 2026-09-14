/**
 * Session share links (1.8).
 *
 *     bebok://share?v=1&ep=<endpoint,endpoint,…>&s=<sessionId>&t=<token>&n=<engine>&fp=<fingerprint>
 *
 * Minted by the desktop (`POST /session/{id}/share`): the token is a device
 * token confined to that one session, `ep` lists the endpoints the desktop
 * is reachable at (tailnet, LAN, relay) so the joiner works from anywhere
 * the pairing would. Pasted into another Bebok (desktop or phone) it becomes
 * a `share` engine target - see `EngineTargetStore` and `ShareStore`.
 */

import { isAllowedRemoteEndpoint } from './endpoint-probe';

export const SHARE_URL_SCHEME = 'bebok:';
export const SHARE_URL_HOST = 'share';
export const SHARE_URL_VERSION = 1;

export interface ShareInvite {
  version: number;
  endpoints: string[];
  sessionId: string;
  token: string;
  engineName: string | null;
  fingerprint: string | null;
}

export type ShareUrlErrorReason =
  'not_share_url' | 'unsupported_version' | 'missing_field' | 'no_endpoints';

export class ShareUrlError extends Error {
  constructor(
    readonly reason: ShareUrlErrorReason,
    message: string,
  ) {
    super(message);
    this.name = 'ShareUrlError';
  }
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Parse a pasted link (surrounding whitespace and chat quoting tolerated). */
export function parseShareUrl(raw: string): ShareInvite {
  const text = (raw ?? '').trim().replace(/^<|>$/g, '');
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    throw new ShareUrlError('not_share_url', 'not a URL');
  }
  const host = url.host || url.pathname.replace(/^\/\//, '').split(/[/?#]/)[0];
  if (url.protocol !== SHARE_URL_SCHEME || host !== SHARE_URL_HOST) {
    throw new ShareUrlError('not_share_url', 'not a bebok://share link');
  }
  const params = url.searchParams;
  const version = Number(params.get('v') ?? '0');
  if (version !== SHARE_URL_VERSION) {
    throw new ShareUrlError(
      'unsupported_version',
      `share link version ${version} is not supported`,
    );
  }
  const sessionId = (params.get('s') ?? '').trim();
  const token = (params.get('t') ?? '').trim();
  if (!UUID.test(sessionId) || token.length < 16) {
    throw new ShareUrlError('missing_field', 'share link is missing the session or token');
  }
  const endpoints = (params.get('ep') ?? '')
    .split(',')
    .map((e) => e.trim().replace(/\/+$/, ''))
    .filter((e) => e && isAllowedRemoteEndpoint(e))
    .filter((e, i, all) => all.indexOf(e) === i);
  if (endpoints.length === 0) {
    throw new ShareUrlError('no_endpoints', 'share link has no usable endpoint');
  }
  return {
    version,
    endpoints,
    sessionId,
    token,
    engineName: params.get('n')?.trim() || null,
    fingerprint: params.get('fp')?.trim() || null,
  };
}

/** Target id of a joined share: one per shared session. */
export function shareTargetId(sessionId: string): string {
  return `share:${sessionId}`;
}
