# Faza 1 — naprawy bez zmiany modelu działania

Typ: `fix:`. Ryzyko: niskie. Zależności: brak.

## 1.1 Prompt przez stdin zamiast argv

Problem: `claude.rs:175-176` dokłada cały transkrypt jako argument po `--`,
`--append-system-prompt` też w argv. Linux: 128 KiB na argument (MAX_ARG_STRLEN),
Windows: 32 767 znaków na linię, przez `cmd /C` 8 191.

Zmiany:
- `bebok-cli-agents/src/backend.rs`
  - `CliBackend::stdin_payload(&self, req: &CliRequest) -> Option<Vec<u8>>`
    (domyślnie `None` = obecne zachowanie).
- `bebok-cli-agents/src/runner.rs`
  - gdy payload jest `Some`: `Stdio::piped()`, zapis w osobnym tasku
    (`tokio::io::AsyncWriteExt::write_all`), potem `drop(stdin)` → EOF.
    Ważne: zamknięcie stdin jest tym, co odblokowuje `claude -p`
    (komentarz w `claude.rs:96-99` opisuje zawieszanie przy otwartym stdin).
  - błąd zapisu (EPIPE, gdy CLI padło) nie może przykryć błędu z stderr/exit.
- `bebok-cli-agents/src/backends/claude.rs`
  - `--input-format stream-json` **[verify]**; jedna linia JSON
    `{"type":"user","message":{"role":"user","content":[...]}}`.
  - pozycyjny prompt i `--` znikają, gdy używamy stdin.
  - system prompt → plik tymczasowy + `--append-system-prompt-file` **[verify]**;
    jeśli flagi brak w probe → fallback na argv z limitem rozmiaru
    i czytelnym błędem `CliError::Config("system prompt too long for argv")`.
  - plik tymczasowy żyje w `TempDir` trzymanym przez runner do końca procesu
    (patrz 07, `PreparedTurn`).
- `bebok-cli-agents/src/transcript.rs`
  - renderowanie do bloków treści Anthropic zamiast jednego stringa:
    tekst + `image` (base64, media_type z `agent/images.rs`).
- Capabilities: `images: true`; w `bebok-core` odblokować
  `model_supports_images` dla providerów CLI z tą capability.

Testy: `claude.rs` — `build_command` nie zawiera transkryptu w argv;
`stdin_payload` zawiera blok `image`; runner — test z fałszywym binarnym
(skrypt `cat` → echo stdin jako JSONL) w `#[cfg(unix)]` i `.cmd` w `#[cfg(windows)]`.

## 1.2 Błędy w `result`

- `claude.rs::parse_line`: `type: "result"` z `is_error: true` albo
  `subtype` ∈ {`error_max_turns`, `error_during_execution`, ...}:
  najpierw `CliEvent::Usage` (koszt nadal się liczy), potem
  `Err(CliError::Exit { message })` z tekstem dla użytkownika.
- `error_max_turns` → komunikat z podpowiedzią `extra.max_turns`.
- `permission_denials` niepuste → dopisać do wiadomości końcowej jako status
  (`StatusPart`), żeby było widać, że CLI czegoś nie mogło zrobić.

Testy: fixture JSONL dla każdego subtype.

## 1.3 Strumieniowanie tokenów

- `claude.rs::build_command`: `--include-partial-messages` **[verify]**.
- `parse_line`: `type: "stream_event"` → `content_block_delta`
  (`text_delta` → `CliEvent::Text`, `thinking_delta` → `CliEvent::Thinking`).
- `CliStreamState`: flaga `saw_partial` na bieżący message id; pełna wiadomość
  `assistant` z tymi samymi blokami tekstu jest wtedy pomijana (bez duplikatu),
  ale nadal przetwarzamy z niej `tool_use` i `usage`.

Testy: fixture z deltami + pełną wiadomością → tekst raz.

## 1.4 Licznik kontekstu i katalog modeli

- `bebok-llm/src/provider.rs` (`Usage`): nowe pole
  `context_tokens: Option<u64>` (rozmiar promptu ostatniego wywołania API).
- `claude.rs`: zapamiętać `usage` z **ostatniej** wiadomości `assistant`
  (input + cache_read + cache_creation) → `context_tokens`; `result.usage`
  i `total_cost_usd` tylko do kosztu.
- `bebok-core/src/context.rs` (`context_used_from_usage`): preferuje
  `context_tokens`, gdy jest.
- `CliBackend::catalog_model(&self, cli_model: &str, init_model: Option<&str>) -> Option<String>`:
  `init.model` z linii `system/init` (np. `claude-opus-5`) → id w
  `model_catalog`; aliasy `opus/sonnet/haiku` jako fallback.
  `context_window_for` korzysta z tego zamiast domyślnych 64k.

Testy: `context.rs` — `context_tokens` wygrywa z sumą; `catalog_model` dla aliasów.

## Pliki

`bebok-cli-agents/src/{backend.rs,runner.rs,transcript.rs,backends/claude.rs}`,
`bebok-llm/src/provider.rs`, `bebok-core/src/context.rs`,
`bebok-core/src/agent/images.rs` (eksport media_type), CHANGELOG.md (`### Fixes`).
