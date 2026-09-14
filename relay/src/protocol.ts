/**
 * Wire format between a Bebok engine and its `Tunnel` Durable Object.
 *
 * The engine keeps ONE WebSocket open to `GET /t/<tunnelId>/engine` (header
 * `Authorization: Bearer <tunnel secret>`). Every HTTP request a phone sends
 * to `/t/<tunnelId>/<path>` becomes a `req` frame; the engine answers with a
 * `res` head, zero or more `chunk` frames (SSE streams for as long as the
 * phone listens) and an `end`. Frames are JSON text; bodies are base64 so a
 * frame is always valid UTF-8 regardless of the payload.
 *
 * The relay never looks inside a request: device tokens and the remote-scope
 * allowlist are enforced by the engine exactly as on its LAN listener.
 */

export type EngineFrame =
  | { type: 'res'; id: string; status: number; headers: [string, string][] }
  | { type: 'chunk'; id: string; data: string }
  | { type: 'end'; id: string }
  | { type: 'error'; id: string; message: string }
  | { type: 'snapshot'; sessionId: string; readers: string[]; data: unknown }
  | { type: 'snapshotDelete'; sessionId: string }
  | { type: 'pong' };

export type RelayFrame =
  | { type: 'req'; id: string; method: string; path: string; headers: [string, string][]; body: string | null }
  | { type: 'cancel'; id: string }
  | { type: 'ping' };

/** Max request body the relay forwards (phones send prompts, not files). */
export const MAX_BODY_BYTES = 1_048_576;
/** Time the engine has to send the `res` head. */
export const HEAD_TIMEOUT_MS = 60_000;
/** Keepalive interval on the engine socket (hibernation-safe). */
export const PING_INTERVAL_MS = 30_000;

/** Hop-by-hop / relay-managed headers never copied in either direction. */
const STRIPPED = new Set([
  'connection',
  'keep-alive',
  'transfer-encoding',
  'upgrade',
  'host',
  'content-length',
  'cf-connecting-ip',
  'cf-ray',
  'cf-visitor',
  'cf-ipcountry',
  'x-forwarded-for',
  'x-forwarded-proto',
  'x-real-ip',
]);

export function forwardableHeaders(headers: Headers): [string, string][] {
  const out: [string, string][] = [];
  headers.forEach((value, name) => {
    if (!STRIPPED.has(name.toLowerCase()) && !name.toLowerCase().startsWith('cf-')) {
      out.push([name, value]);
    }
  });
  return out;
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = '';
  const step = 0x8000;
  for (let i = 0; i < bytes.length; i += step) {
    binary += String.fromCharCode(...bytes.subarray(i, i + step));
  }
  return btoa(binary);
}

export function base64ToBytes(text: string): Uint8Array {
  const binary = atob(text);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

export async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
}

export function timingSafeEqual(a: string, b: string): boolean {
  if (a.length !== b.length) {
    return false;
  }
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) {
    diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  }
  return diff === 0;
}
