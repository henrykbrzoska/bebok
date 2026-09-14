/**
 * End-to-end through workerd: a fake engine dials the tunnel over WebSocket
 * and answers the frames the DO forwards; the "phone" is plain `fetch`.
 */

import { SELF } from 'cloudflare:test';
import { describe, expect, it } from 'vitest';

import type { EngineFrame, RelayFrame } from '../src/protocol';

const TUNNEL = 'abcdefghijklmnop0123456789';
const SECRET = 's'.repeat(40);

interface FakeEngine {
  ws: WebSocket;
  frames: RelayFrame[];
  next(): Promise<RelayFrame>;
  send(frame: EngineFrame): void;
  close(): void;
}

async function connectEngine(secret = SECRET, tunnel = TUNNEL): Promise<{ status: number; engine: FakeEngine | null }> {
  const res = await SELF.fetch(`https://relay.test/t/${tunnel}/engine`, {
    headers: { upgrade: 'websocket', authorization: `Bearer ${secret}` },
  });
  if (res.status !== 101 || !res.webSocket) {
    return { status: res.status, engine: null };
  }
  const ws = res.webSocket;
  ws.accept();
  const frames: RelayFrame[] = [];
  const waiters: ((f: RelayFrame) => void)[] = [];
  ws.addEventListener('message', (event) => {
    const frame = JSON.parse(String(event.data)) as RelayFrame;
    const waiter = waiters.shift();
    if (waiter) {
      waiter(frame);
    } else {
      frames.push(frame);
    }
  });
  const engine: FakeEngine = {
    ws,
    frames,
    next: () =>
      frames.length > 0
        ? Promise.resolve(frames.shift()!)
        : new Promise((resolve) => waiters.push(resolve)),
    send: (frame) => ws.send(JSON.stringify(frame)),
    close: () => ws.close(1000, 'test done'),
  };
  return { status: 101, engine };
}

const b64 = (text: string) => btoa(text);

describe('relay worker', () => {
  it('answers health and rejects bad tunnel ids', async () => {
    expect((await SELF.fetch('https://relay.test/health')).status).toBe(200);
    expect((await SELF.fetch('https://relay.test/t/short/session')).status).toBe(400);
    expect((await SELF.fetch('https://relay.test/nope')).status).toBe(404);
  });

  it('reports engine_offline when no engine is attached', async () => {
    const res = await SELF.fetch(`https://relay.test/t/${'z'.repeat(20)}/session`);
    expect(res.status).toBe(503);
    expect(await res.json()).toEqual({ error: 'engine_offline' });
    expect(res.headers.get('access-control-allow-origin')).toBe('*');
  });

  it('claims the tunnel on first use and refuses another secret afterwards', async () => {
    const tunnel = 'claimclaimclaimclaim1';
    const first = await connectEngine('a'.repeat(40), tunnel);
    expect(first.status).toBe(101);
    first.engine!.close();
    const wrong = await connectEngine('b'.repeat(40), tunnel);
    expect(wrong.status).toBe(403);
    const again = await connectEngine('a'.repeat(40), tunnel);
    expect(again.status).toBe(101);
    again.engine!.close();
    expect((await connectEngine('short', tunnel)).status).toBe(401);
  });

  it('forwards a phone request to the engine and streams the answer back', async () => {
    const { engine } = await connectEngine();
    expect(engine).not.toBeNull();

    const phone = SELF.fetch(`https://relay.test/t/${TUNNEL}/session?directory=%2Ftmp`, {
      method: 'POST',
      headers: { authorization: 'Bearer device-token', 'content-type': 'application/json' },
      body: JSON.stringify({ agent: 'code' }),
    });

    const req = await engine!.next();
    expect(req.type).toBe('req');
    if (req.type !== 'req') return;
    expect(req.method).toBe('POST');
    expect(req.path).toBe('/session?directory=%2Ftmp');
    expect(req.headers).toContainEqual(['authorization', 'Bearer device-token']);
    expect(req.headers.find(([k]) => k === 'host')).toBeUndefined();
    expect(atob(req.body!)).toBe('{"agent":"code"}');

    engine!.send({ type: 'res', id: req.id, status: 201, headers: [['content-type', 'application/json']] });
    engine!.send({ type: 'chunk', id: req.id, data: b64('{"id":"s1"') });
    engine!.send({ type: 'chunk', id: req.id, data: b64('}') });
    engine!.send({ type: 'end', id: req.id });

    const res = await phone;
    expect(res.status).toBe(201);
    expect(res.headers.get('x-bebok-relay')).toBe('tunnel');
    expect(await res.json()).toEqual({ id: 's1' });
    engine!.close();
  });

  it('delivers SSE chunks as they arrive', async () => {
    const { engine } = await connectEngine();
    const phone = SELF.fetch(`https://relay.test/t/${TUNNEL}/event`, { headers: { accept: 'text/event-stream' } });
    const req = await engine!.next();
    if (req.type !== 'req') throw new Error('expected req');
    engine!.send({ type: 'res', id: req.id, status: 200, headers: [['content-type', 'text/event-stream']] });
    engine!.send({ type: 'chunk', id: req.id, data: b64('id: 1\nevent: session.updated\ndata: {}\n\n') });

    const res = await phone;
    expect(res.status).toBe(200);
    const reader = res.body!.getReader();
    const first = await reader.read();
    expect(new TextDecoder().decode(first.value)).toContain('event: session.updated');

    engine!.send({ type: 'chunk', id: req.id, data: b64('id: 2\ndata: {}\n\n') });
    const second = await reader.read();
    expect(new TextDecoder().decode(second.value)).toContain('id: 2');

    engine!.send({ type: 'end', id: req.id });
    expect((await reader.read()).done).toBe(true);
    engine!.close();
  });

  it('turns an engine error frame into a 502', async () => {
    const { engine } = await connectEngine();
    const phone = SELF.fetch(`https://relay.test/t/${TUNNEL}/session`);
    const req = await engine!.next();
    if (req.type !== 'req') throw new Error('expected req');
    engine!.send({ type: 'error', id: req.id, message: 'router panicked' });
    const res = await phone;
    expect(res.status).toBe(502);
    expect(await res.json()).toMatchObject({ error: 'relay_upstream', message: 'router panicked' });
    engine!.close();
  });

  it('rejects bodies over the limit and websocket upgrades', async () => {
    const { engine } = await connectEngine();
    const big = await SELF.fetch(`https://relay.test/t/${TUNNEL}/session`, { method: 'POST', body: 'x'.repeat(1_048_577) });
    expect(big.status).toBe(413);
    const ws = await SELF.fetch(`https://relay.test/t/${TUNNEL}/pty/1/connect`, { headers: { upgrade: 'websocket' } });
    expect(ws.status).toBe(400);
    engine!.close();
  });

  it('serves cloud snapshots to listed readers even after the engine left', async () => {
    const tunnel = 'cloudcloudcloudcloud1';
    const { engine } = await connectEngine(SECRET, tunnel);
    const token = 'device-token-1';
    const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(token));
    const reader = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, '0')).join('');
    engine!.send({
      type: 'snapshot',
      sessionId: 's-42',
      readers: [reader],
      data: { meta: { title: 'Fix the tests' }, messages: [{ role: 'user', text: 'hi' }] },
    });
    // Let the DO persist it before the engine goes away.
    await new Promise((resolve) => setTimeout(resolve, 50));
    engine!.close();
    await new Promise((resolve) => setTimeout(resolve, 50));

    const offline = await SELF.fetch(`https://relay.test/t/${tunnel}/session`);
    expect(offline.status).toBe(503);

    const list = await SELF.fetch(`https://relay.test/t/${tunnel}/cloud/sessions`, { headers: { authorization: `Bearer ${token}` } });
    expect(list.status).toBe(200);
    const body = (await list.json()) as { engineOnline: boolean; sessions: { sessionId: string; meta: { title: string } }[] };
    expect(body.engineOnline).toBe(false);
    expect(body.sessions.map((s) => s.sessionId)).toEqual(['s-42']);
    expect(body.sessions[0].meta.title).toBe('Fix the tests');

    const one = await SELF.fetch(`https://relay.test/t/${tunnel}/cloud/sessions/s-42`, { headers: { authorization: `Bearer ${token}` } });
    expect(one.status).toBe(200);
    expect(((await one.json()) as { messages: unknown[] }).messages).toHaveLength(1);

    const stranger = await SELF.fetch(`https://relay.test/t/${tunnel}/cloud/sessions`, { headers: { authorization: 'Bearer other' } });
    expect(((await stranger.json()) as { sessions: unknown[] }).sessions).toEqual([]);
    expect((await SELF.fetch(`https://relay.test/t/${tunnel}/cloud/sessions/s-42`, { headers: { authorization: 'Bearer other' } })).status).toBe(404);
    expect((await SELF.fetch(`https://relay.test/t/${tunnel}/cloud/sessions`)).status).toBe(401);
  });
});
