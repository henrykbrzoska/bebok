# bebok-relay

Cloudflare Worker + Durable Object that lets a paired phone reach a Bebok
engine at home from anywhere: the engine keeps an outbound WebSocket to its
`Tunnel` object and every HTTPS request the phone sends to
`/t/<tunnelId>/<path>` is forwarded over that socket (reverse tunnel). The
relay never inspects requests - device tokens and the remote-scope allowlist
are enforced by the engine exactly as on its LAN listener.

```
phone ── https ──▶ worker ──▶ Tunnel DO ── ws (engine-initiated) ──▶ engine (home)
```

| Route | Who | What |
|---|---|---|
| `GET /health` | anyone | `ok` |
| `GET /t/<id>/engine` | engine | WebSocket; `Authorization: Bearer <tunnel secret>`; first caller claims the id (TOFU), later callers must match |
| `ANY /t/<id>/<path>` | phone | forwarded to the engine; `503 {"error":"engine_offline"}` when it is not connected |
| `GET /t/<id>/cloud/sessions[/<sid>]` | phone | session snapshots the engine pushed, served by the DO even when the engine is offline (visible to device tokens the engine listed as readers) |
| `GET /t/<id>/relay/status` | phone | `{engineOnline, pending}` |

Wire format: `src/protocol.ts` (JSON frames, base64 bodies, SSE = chunks).

## Develop

```bash
npm install --legacy-peer-deps     # npm 10 trips over the vitest peer edge without the flag
npm test                           # vitest inside workerd (no account needed)
npm run typecheck
npm run dev                        # http://localhost:8787
```

## Deploy

```bash
npx wrangler login                 # once, opens the browser
npm run deploy                     # -> https://bebok-relay.<your-subdomain>.workers.dev
```

Free plan is enough for personal use (Durable Objects with SQLite storage
are included). Nothing has to be configured on the Cloudflare side: the
engine generates its tunnel secret and claims its tunnel id on first
connect. Put the worker URL into the desktop app (Settings -> Remote ->
Relay) and the QR code advertises it next to the LAN/tailnet endpoints.

`compatibility_date` is pinned to what the local `workerd` in
`@cloudflare/vitest-pool-workers` supports; bump it together with the dev
dependencies.
