# Faza 5 — prawdziwe wiersze narzędzi dla narzędzi CLI

Typ: `feat:`. Zależności: 07 (hooki dają pełne input/output), opcjonalnie
działa też z samego JSONL (gorsze dane).

## Silnik

- `bebok-llm` `StreamEvent::ExternalTool { id, name, input, output, is_error,
  source: ExternalSource::Cli }` — **nigdy nie wykonywane** przez pętlę.
- Źródła:
  1. hook `PostToolUse` (07) → silnik → zdarzenie do bieżącej tury (pełne dane),
  2. fallback: `claude.rs` łączy `tool_use` z `tool_result` po `tool_use_id`
     (dziś: `ToolActivity` w Thinking, `DETAIL_CAP = 200`).
  Deduplikacja po `tool_use_id`.
- `agent/turn.rs`: zapis jako część `tool` z `external: true`.
- Dla Edit/Write/MultiEdit: diff z change trackingu (07 snapshotuje
  w `PreToolUse`) → ten sam komponent diff co dla `edit_file`.

## Zgodność wsteczna transkryptu

- `bebok-llm/src/wire.rs` i `bebok-cli-agents/src/transcript.rs`: części
  `external` przy przejściu sesji na HTTP renderowane jako tekst
  (`[Claude Code ran Bash: ...] → wynik`), nigdy jako `tool_use`
  (API odrzuca osierocone wywołania).

## Klient

- `client/src/core/engine.dtos.ts`: `external?: boolean` w ToolPart.
- `client/src/views/chat/parts/tool-part.ts` / `tool-run-row`: znaczek
  „wykonane przez Claude Code”, brak przycisku retry, `data-testid="tool-external-badge"`.
- i18n: nowy klucz `chat.tool.externalBy` we wszystkich 12 słownikach
  (`client/src/i18n/*.ts`, append-only).
- spec: `tool-part.spec.ts`.

## Pliki

`bebok-llm/src/{provider.rs,wire.rs}`, `bebok-core/src/agent/turn.rs`,
`bebok-cli-agents/src/{backend.rs,provider.rs,transcript.rs,backends/claude.rs}`,
`client/src/{core/engine.dtos.ts,views/chat/parts/tool-part.ts,i18n/*.ts}`.
