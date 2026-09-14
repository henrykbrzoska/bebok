/**
 * `Tunnel` Durable Object - one per engine, keyed by the engine's install id.
 *
 * - `engine` socket: exactly one at a time, authenticated with the tunnel
 *   secret. First connection claims the tunnel (TOFU): the secret's sha256 is
 *   stored and every later connection must present the same one. Uses the
 *   WebSocket hibernation API so an idle tunnel costs nothing.
 * - phone requests: `fetch()` serialises them into `req` frames, waits for the
 *   `res` head (60 s) and streams `chunk`s into the response body until `end`.
 *   SSE therefore just works. With no engine attached the answer is
 *   `503 {"error":"engine_offline"}` so the phone can say "desktop is off".
 * - cloud snapshots (1.8 "cloud chats"): the engine pushes `snapshot` frames
 *   for sessions the user marked; `GET /cloud/sessions[/<id>]` serves them
 *   from storage even while the engine is offline, to callers whose device
 *   token hash the engine listed as a reader.
 */

import { DurableObject } from 'cloudflare:workers';

import {
  base64ToBytes,
  bytesToBase64,
  EngineFrame,
  forwardableHeaders,
  HEAD_TIMEOUT_MS,
  MAX_BODY_BYTES,
  RelayFrame,
  sha256Hex,
  timingSafeEqual,
} from './protocol';

interface Pending {
  resolveHead: (head: { status: number; headers: [string, string][] }) => void;
  rejectHead: (reason: Error) => void;
  writer: WritableStreamDefaultWriter<Uint8Array>;
  headTimer: ReturnType<typeof setTimeout>;
}

interface StoredSnapshot {
  sessionId: string;
  readers: string[];
  updatedAt: number;
  data: unknown;
}

const json = (status: number, body: unknown, extra: Record<string, string> = {}) =>
  new Response(JSON.stringify(body), {
    status,
    headers: { 'content-type': 'application/json', 'cache-control': 'no-store', ...extra },
  });

export class Tunnel extends DurableObject<Env> {
  private readonly pending = new Map<string, Pending>();

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    // The engine's keepalive is answered by the runtime without waking a
    // hibernated object.
    ctx.setWebSocketAutoResponse(
      new WebSocketRequestResponsePair(JSON.stringify({ type: 'ping' }), JSON.stringify({ type: 'pong' })),
    );
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    // The worker strips `/t/<id>`; we see `/engine`, `/cloud/...` or the engine path.
    if (url.pathname === '/engine') {
      return this.acceptEngine(request);
    }
    if (url.pathname === '/cloud/sessions' || url.pathname.startsWith('/cloud/sessions/')) {
      return this.serveCloud(request, url);
    }
    if (url.pathname === '/relay/status') {
      return json(200, { engineOnline: this.engineSocket() !== null, pending: this.pending.size });
    }
    return this.forward(request, url);
  }

  // ---------------------------------------------------------------------------
  // engine side
  // ---------------------------------------------------------------------------

  private engineSocket(): WebSocket | null {
    const sockets = this.ctx.getWebSockets('engine');
    return sockets.length > 0 ? sockets[0] : null;
  }

  private async acceptEngine(request: Request): Promise<Response> {
    if (request.headers.get('upgrade')?.toLowerCase() !== 'websocket') {
      return json(426, { error: 'upgrade_required' });
    }
    const presented = request.headers.get('authorization')?.replace(/^Bearer\s+/i, '') ?? '';
    if (presented.length < 32) {
      return json(401, { error: 'bad_secret' });
    }
    const presentedHash = await sha256Hex(presented);
    const stored = await this.ctx.storage.get<string>('secretHash');
    if (stored === undefined) {
      // Trust on first use: the first engine to dial in owns this tunnel id.
      await this.ctx.storage.put('secretHash', presentedHash);
      await this.ctx.storage.put('claimedAt', Date.now());
    } else if (!timingSafeEqual(stored, presentedHash)) {
      return json(403, { error: 'wrong_secret' });
    }

    // One engine per tunnel: a reconnect replaces the stale socket.
    for (const old of this.ctx.getWebSockets('engine')) {
      try {
        old.close(1012, 'replaced by a new engine connection');
      } catch {
        /* already gone */
      }
    }
    this.failAllPending(new Error('engine reconnected'));

    const pair = new WebSocketPair();
    const [client, server] = [pair[0], pair[1]];
    this.ctx.acceptWebSocket(server, ['engine']);
    server.serializeAttachment({ connectedAt: Date.now() });
    await this.ctx.storage.put('lastConnectedAt', Date.now());
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    if (typeof message !== 'string') {
      return;
    }
    let frame: EngineFrame;
    try {
      frame = JSON.parse(message) as EngineFrame;
    } catch {
      return;
    }
    switch (frame.type) {
      case 'res': {
        const pending = this.pending.get(frame.id);
        if (!pending) {
          this.send(ws, { type: 'cancel', id: frame.id });
          return;
        }
        clearTimeout(pending.headTimer);
        pending.resolveHead({ status: frame.status, headers: frame.headers });
        return;
      }
      case 'chunk': {
        const pending = this.pending.get(frame.id);
        if (!pending) {
          return;
        }
        try {
          await pending.writer.write(base64ToBytes(frame.data));
        } catch {
          // The phone went away: tell the engine to stop streaming.
          this.pending.delete(frame.id);
          this.send(ws, { type: 'cancel', id: frame.id });
        }
        return;
      }
      case 'end': {
        const pending = this.pending.get(frame.id);
        this.pending.delete(frame.id);
        try {
          await pending?.writer.close();
        } catch {
          /* already closed */
        }
        return;
      }
      case 'error': {
        const pending = this.pending.get(frame.id);
        this.pending.delete(frame.id);
        if (pending) {
          clearTimeout(pending.headTimer);
          pending.rejectHead(new Error(frame.message));
          try {
            await pending.writer.abort(frame.message);
          } catch {
            /* ignore */
          }
        }
        return;
      }
      case 'snapshot': {
        const snapshot: StoredSnapshot = {
          sessionId: frame.sessionId,
          readers: frame.readers,
          updatedAt: Date.now(),
          data: frame.data,
        };
        await this.ctx.storage.put(`session:${frame.sessionId}`, snapshot);
        return;
      }
      case 'snapshotDelete': {
        await this.ctx.storage.delete(`session:${frame.sessionId}`);
        return;
      }
      case 'pong':
        return;
    }
  }

  async webSocketClose(): Promise<void> {
    this.failAllPending(new Error('engine disconnected'));
    await this.ctx.storage.put('lastDisconnectedAt', Date.now());
  }

  async webSocketError(): Promise<void> {
    this.failAllPending(new Error('engine socket error'));
  }

  private send(ws: WebSocket, frame: RelayFrame): void {
    try {
      ws.send(JSON.stringify(frame));
    } catch {
      /* socket closing */
    }
  }

  private failAllPending(reason: Error): void {
    for (const [id, pending] of this.pending) {
      clearTimeout(pending.headTimer);
      pending.rejectHead(reason);
      void pending.writer.abort(reason.message).catch(() => undefined);
      this.pending.delete(id);
    }
  }

  // ---------------------------------------------------------------------------
  // phone side
  // ---------------------------------------------------------------------------

  private async forward(request: Request, url: URL): Promise<Response> {
    const engine = this.engineSocket();
    if (!engine) {
      return json(503, { error: 'engine_offline' }, { 'retry-after': '10' });
    }
    if (request.headers.get('upgrade')) {
      return json(400, { error: 'websocket_not_relayed' });
    }

    let body: string | null = null;
    if (request.method !== 'GET' && request.method !== 'HEAD') {
      const bytes = new Uint8Array(await request.arrayBuffer());
      if (bytes.byteLength > MAX_BODY_BYTES) {
        return json(413, { error: 'body_too_large' });
      }
      body = bytesToBase64(bytes);
    }

    const id = crypto.randomUUID();
    const { readable, writable } = new TransformStream<Uint8Array, Uint8Array>();
    const writer = writable.getWriter();

    const head = new Promise<{ status: number; headers: [string, string][] }>((resolve, reject) => {
      const pending: Pending = {
        resolveHead: resolve,
        rejectHead: reject,
        writer,
        headTimer: setTimeout(() => {
          this.pending.delete(id);
          reject(new Error('engine did not answer in time'));
          this.send(engine, { type: 'cancel', id });
        }, HEAD_TIMEOUT_MS),
      };
      this.pending.set(id, pending);
    });

    this.send(engine, {
      type: 'req',
      id,
      method: request.method,
      path: url.pathname + url.search,
      headers: forwardableHeaders(request.headers),
      body,
    });

    // A phone that disconnects mid-stream (SSE) must not leave the engine
    // streaming into the void.
    request.signal.addEventListener('abort', () => {
      if (this.pending.delete(id)) {
        this.send(engine, { type: 'cancel', id });
        void writer.abort('client aborted').catch(() => undefined);
      }
    });

    let resolved: { status: number; headers: [string, string][] };
    try {
      resolved = await head;
    } catch (err) {
      void writer.abort(String(err)).catch(() => undefined);
      const message = err instanceof Error ? err.message : String(err);
      return json(502, { error: 'relay_upstream', message }, { 'retry-after': '5' });
    }

    const headers = new Headers();
    for (const [name, value] of resolved.headers) {
      headers.append(name, value);
    }
    headers.set('cache-control', 'no-store');
    headers.set('x-bebok-relay', 'tunnel');
    return new Response(request.method === 'HEAD' ? null : readable, { status: resolved.status, headers });
  }

  // ---------------------------------------------------------------------------
  // cloud snapshots (served even when the engine is offline)
  // ---------------------------------------------------------------------------

  private async serveCloud(request: Request, url: URL): Promise<Response> {
    if (request.method !== 'GET') {
      return json(405, { error: 'method_not_allowed' });
    }
    const token = request.headers.get('authorization')?.replace(/^Bearer\s+/i, '') ?? url.searchParams.get('token') ?? '';
    if (!token) {
      return json(401, { error: 'unauthorized' });
    }
    const reader = await sha256Hex(token);
    const stored = await this.ctx.storage.list<StoredSnapshot>({ prefix: 'session:' });
    const visible = [...stored.values()].filter((s) => s.readers.some((r) => timingSafeEqual(r, reader)));

    if (url.pathname === '/cloud/sessions') {
      const list = visible
        .map((s) => ({ sessionId: s.sessionId, updatedAt: s.updatedAt, meta: (s.data as { meta?: unknown })?.meta ?? null }))
        .sort((a, b) => b.updatedAt - a.updatedAt);
      return json(200, { engineOnline: this.engineSocket() !== null, sessions: list });
    }
    const sessionId = decodeURIComponent(url.pathname.slice('/cloud/sessions/'.length));
    const snapshot = visible.find((s) => s.sessionId === sessionId);
    if (!snapshot) {
      // Same answer for "no such session" and "not yours".
      return json(404, { error: 'not_found' });
    }
    return json(200, { engineOnline: this.engineSocket() !== null, updatedAt: snapshot.updatedAt, ...(snapshot.data as object) });
  }
}

export interface Env {
  TUNNEL: DurableObjectNamespace<Tunnel>;
}
