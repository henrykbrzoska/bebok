/**
 * Engine fetch wrapper + the engine capability token store (F0-5).
 *
 * The engine mints one random token per launch and refuses every API request
 * that does not present it (`Authorization: Bearer <token>`). The token reaches
 * the client inside the `BEBOK_READY` URL announced by the engine — see
 * `transport.strategy.ts`, which splits it off and calls `setEngineToken()`
 * before any request is made.
 *
 * This wrapper is the single choke point for outgoing engine traffic (REST and
 * the SSE stream, which is read with `fetch` and therefore can carry headers),
 * so attaching the header here covers every caller.
 */

/** The token for the engine we are currently attached to (memory only). */
let engineToken: string | null = null;

/** Store the capability token handed to us by the engine handshake. */
export function setEngineToken(token: string | null): void {
  engineToken = token && token.length > 0 ? token : null;
}

/** The current capability token, or null when the engine issued none. */
export function getEngineToken(): string | null {
  return engineToken;
}

/**
 * Split a `…?token=<t>` engine URL into its base URL and the token.
 *
 * The engine announces `BEBOK_READY http://host:port/?token=…`; every launcher
 * (Tauri sidecar reader, Android `EngineLauncherPlugin`, or a human pasting the
 * line into the connect view) forwards that string verbatim, so this is where
 * the two parts are separated. URLs without a token pass through unchanged —
 * an engine started with `BEBOK_NO_AUTH=1` announces a plain URL.
 */
export function splitEngineUrl(raw: string): { baseUrl: string; token: string | null } {
  const trimmed = (raw ?? '').trim();
  const cut = trimmed.indexOf('?');
  if (cut < 0) {
    return { baseUrl: stripTrailingSlash(trimmed), token: null };
  }
  const base = stripTrailingSlash(trimmed.slice(0, cut));
  const params = new URLSearchParams(trimmed.slice(cut + 1));
  const token = params.get('token');
  return { baseUrl: base, token: token && token.length > 0 ? token : null };
}

function stripTrailingSlash(url: string): string {
  return url.endsWith('/') ? url.slice(0, -1) : url;
}

/**
 * fetch() with the engine capability token and a JSON Content-Type header when
 * a body is supplied.
 */
export async function authFetch(url: string, init: RequestInit = {}): Promise<Response> {
  const headers = new Headers(init.headers);
  if (init.body !== undefined && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }
  if (engineToken && !headers.has('Authorization')) {
    headers.set('Authorization', `Bearer ${engineToken}`);
  }
  return fetch(url, { ...init, headers });
}
