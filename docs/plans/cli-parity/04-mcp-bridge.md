# Faza 4 — narzędzia Beboka dla CLI przez MCP przypięty do sesji

Typ: `feat:`. Zależności: 07 (token na turę, generator `mcp.json`), 03.
Po 07 ta faza jest **opcjonalna**: uprawnienia i Changes dają hooki.
Wartość 04 = CLI korzysta z narzędzi Beboka (code_index, code_ast, browser_*,
ssh, fetch), z gwarancjami `exec.rs`.

## 4.1 Wydzielenie JSON-RPC

- `bebok-server/src/mcp_server.rs` → `bebok-server/src/mcp/`:
  `protocol.rs` (`handle_message`, `list_tools`), `stdio.rs` (obecny tryb),
  `http.rs` (nowy). Zachowanie `bebok-server mcp` bez zmian (test regresji).

## 4.2 Trasa

- `routes/mod.rs`: `POST /mcp/session/{id}` (streamable HTTP; GET dla SSE, jeśli
  wymagane przez klienta **[verify]**).
- Autoryzacja: token turowy z 07 (`TurnToken`, zakres = sesja + tura, TTL = czas
  tury), **nie** główny token silnika. Middleware `auth.rs` dostaje wyjątek
  dla tej trasy, który sprawdza token turowy.

## 4.3 Wykonanie przez `exec.rs`

- `bebok-core/src/agent/exec.rs`: wyciągnąć `execute_one(ctx: &ExecCtx, call: ToolCall)
  -> ToolOutcome` (gate → build_test_gate → permission → popup → snapshot
  → run → cap → hooki pluginów). Pętla tur woła to samo.
- `call_tool` w trybie HTTP: `execute_one` + publikacja części `tool`
  w bieżącej wiadomości asystenta (`message.part.updated`) → prawdziwe
  tool-rows i diffy w UI.
- Niewystawiane: `task`, `fleet`, `task_*`, narzędzia z MCP projektu.

## 4.4 Profil

- 07 generuje `mcp.json` z `{"bebok": {"type":"http","url":".../mcp/session/<id>",
  "headers":{"Authorization":"Bearer <turn token>"}}}` + `--strict-mcp-config`.
- `extra.tools`: `"native"` (domyślnie po 07) | `"bebok"` | `"both"`.
  `bebok` → wbudowane mutujące narzędzia CLI wyłączone (`--disallowedTools`
  albo `--tools` **[verify]**), `--allowedTools mcp__bebok__*`.

## Ryzyka

- refaktor `exec.rs` = serce pętli; regresja przez `ScriptProvider` w `agent/turn.rs`.
- przerwanie tury (cancel) musi unieważnić token i anulować trwające wywołania.

## Pliki

`bebok-server/src/{mcp_server.rs → mcp/,routes/mod.rs,auth.rs}`,
`bebok-core/src/agent/{exec.rs,turn.rs}`,
`bebok-cli-agents/src/backends/claude.rs`, profil (07).
