# CLI parity — plan

Cel: provider `kind: "cli"` (dziś: Claude Code) ma działać w Beboku tak samo dobrze
jak provider HTTP — długie sesje, uprawnienia, Changes panel, prawdziwe wiersze
narzędzi, licznik kontekstu, obrazy.

> Flagi i pola ustawień Claude Code oznaczone **[verify]** są podane z pamięci —
> przed implementacją potwierdzić je na zainstalowanej wersji (`claude --help`).
> Kod potwierdza dziś tylko flagi z `backends/claude.rs` (wersja 2.1.318).

## Diagnoza (stan 1.10.0)

| # | Problem | Kod |
|---|---|---|
| 1 | Transkrypt jako jeden argument argv, system prompt jako argument | `bebok-cli-agents/src/backends/claude.rs:100-179`, `runner.rs` (stdin null) |
| 2 | Brak `--resume`, pełny transkrypt co turę, wyniki narzędzi ucinane do 500 zn. | `provider.rs` (TODO), `transcript.rs` |
| 3 | System prompt opisuje narzędzia Beboka, których CLI nie ma | `bebok-server/src/services/turn.rs` (`assemble_prompt`), `agent/index_prompt.rs` |
| 4 | Narzędzia CLI omijają permission engine i change tracking | docs/cli-agents.md |
| 5 | Aktywność narzędzi = tekst w Thinking | `provider.rs` (`ToolActivity`) |
| 6 | Brak strumieniowania tokenów | `claude.rs` (brak `--include-partial-messages`) |
| 7 | Licznik kontekstu z `result.usage` (suma pętli), brak modelu w katalogu | `agent/turn.rs`, `context.rs`, `model_catalog` |
| 8 | Obrazy → `[image omitted]` | `transcript.rs` |
| 9 | `result.is_error` traktowany jak sukces | `claude.rs` (`parse_line`) |

## Fazy

| Plik | Faza | Zamyka | Typ PR |
|---|---|---|---|
| [01-fixes.md](01-fixes.md) | stdin, błędy, streaming, licznik | 1, 6, 7, 8, 9 | `fix:` |
| [07-cli-profile.md](07-cli-profile.md) | zarządzany profil Claude Code + hooki | 3 (część), 4, 5 | `feat:` |
| [02-resume.md](02-resume.md) | sesja CLI i `--resume` | 2 | `feat:` |
| [03-transport-prompt.md](03-transport-prompt.md) | prompt świadomy transportu | 3 | `fix:` |
| [04-mcp-bridge.md](04-mcp-bridge.md) | narzędzia Beboka dla CLI przez MCP | 4, dostęp do code_index/browser/ssh | `feat:` |
| [05-native-tool-rows.md](05-native-tool-rows.md) | `ExternalTool` w transkrypcie i UI | 5 | `feat:` |
| [06-docs-ui.md](06-docs-ui.md) | dokumentacja, Settings, changelog | — | `docs:` / `feat:` |

## Zalecana kolejność (zmieniona po dodaniu profilu)

1. **01** — realny bug: długie sesje się psują (E2BIG / limit linii na Windows).
2. **07** — profil + hooki `PreToolUse`/`PostToolUse` dają uprawnienia, Changes
   i pełne wiersze narzędzi **bez** przepisywania narzędzi CLI na MCP. Dostarcza
   też infrastrukturę (katalog profilu, env, pliki tymczasowe, token na turę),
   z której korzystają 02 i 04.
3. **02** i **03** równolegle (tanie).
4. **05** — warstwa prezentacji, zasilana z hooków (07).
5. **04** — opcjonalnie, gdy CLI ma używać narzędzi Beboka (code_index, browser,
   ssh), a nie tylko swoich.
6. **06** — w każdym PR aktualizacja docs/changelog; na koniec UI ustawień.

## Zasady wspólne dla wszystkich faz

- CLI nigdy nie emituje `StreamEvent::ToolCall` (docs/cli-agents.md). Nowe
  zdarzenia są tylko prezentacyjne albo przychodzą z serwera (hook/MCP).
- Testy na nagranych liniach JSONL i fixture'ach, bez prawdziwego `claude`.
- Każda zmiana w runnerze z `#[cfg(windows)]` → `npm run check:windows`.
- `bypassPermissions` / `--dangerously-skip-permissions` nadal zakazane.
- Każda flaga Claude Code musi być bramkowana wynikiem probe (07, krok 2), nie
  zakładana.
