/**
 * Bebok relay worker.
 *
 *   GET  /health                    -> ok
 *   GET  /t/<tunnelId>/engine       -> WebSocket for the engine (Bearer tunnel secret)
 *   ANY  /t/<tunnelId>/<path>       -> forwarded to that engine over its socket
 *   GET  /t/<tunnelId>/cloud/...    -> cloud snapshots, served by the DO itself
 *
 * `tunnelId` is the engine's install id (16-64 url-safe chars). Everything
 * under a tunnel id is handled by that id's `Tunnel` Durable Object; the
 * worker only routes.
 */

import { Tunnel } from './tunnel';
import type { Env } from './tunnel';

export { Tunnel };

const TUNNEL_ID = /^[A-Za-z0-9_-]{16,64}$/;

const CORS: Record<string, string> = {
  'access-control-allow-origin': '*',
  'access-control-allow-methods': 'GET,HEAD,POST,PUT,PATCH,DELETE,OPTIONS',
  'access-control-allow-headers': 'authorization,content-type,last-event-id,x-bebok-device',
  'access-control-expose-headers': 'x-bebok-relay,retry-after',
  'access-control-max-age': '86400',
};

function withCors(response: Response): Response {
  const headers = new Headers(response.headers);
  for (const [k, v] of Object.entries(CORS)) {
    headers.set(k, v);
  }
  return new Response(response.body, { status: response.status, headers, webSocket: response.webSocket ?? undefined });
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const url = new URL(request.url);

    if (request.method === 'OPTIONS') {
      return new Response(null, { status: 204, headers: CORS });
    }
    if (url.pathname === '/health') {
      return withCors(new Response('ok', { headers: { 'content-type': 'text/plain' } }));
    }

    const match = url.pathname.match(/^\/t\/([^/]+)(\/.*)?$/);
    if (!match) {
      return withCors(Response.json({ error: 'not_found' }, { status: 404 }));
    }
    const [, tunnelId, rest] = match;
    if (!TUNNEL_ID.test(tunnelId)) {
      return withCors(Response.json({ error: 'bad_tunnel_id' }, { status: 400 }));
    }

    const stub = env.TUNNEL.get(env.TUNNEL.idFromName(tunnelId));
    const inner = new URL(request.url);
    inner.pathname = rest && rest !== '/' ? rest : '/';
    const forwarded = new Request(inner.toString(), request);
    const response = await stub.fetch(forwarded);
    // 101 responses carry the socket; never rebuild them.
    return response.status === 101 ? response : withCors(response);
  },
} satisfies ExportedHandler<Env>;
