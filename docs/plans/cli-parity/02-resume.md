# Faza 2 — sesja CLI i `--resume`

Typ: `feat:`. Zależności: 01 (stdin), 07 (opcjonalnie `--session-id`).

## Model danych

- `bebok-core/src/session/persist.rs`: w meta sesji
  `cli_session: Option<CliSessionMeta>`:
  `{ key: String, provider: String, model: String, system_hash: String,
     msg_index: usize, profile_hash: String }`.
  `serde(default)` → stare sesje się wczytują.
- `bebok-llm` `ChatRequest` (albo `ProviderCtx` w `bebok-core/src/provider.rs`):
  `resume: Option<ResumeHint { key, from_msg }>`. Preferowany `ProviderCtx`,
  żeby nie dotykać providerów HTTP.

## Przepływ

1. `services/turn.rs`: przed `build_provider_for` porównuje `cli_session`
   z bieżącym (provider, model, hash system promptu, hash profilu z 07).
   Zgodne → `ResumeHint`.
2. `CliProvider::stream`: z hintem wysyła na stdin tylko wiadomości
   `[from_msg..]` + `--resume <key>`; bez hinta pełny transkrypt.
3. Z linii `system/init` czytamy `session_id` → zdarzenie
   `CliEvent::Session(key)` → `turn.rs` zapisuje `cli_session`
   z `msg_index = len(messages)` po turze.
4. Alternatywa **[verify]**: `--session-id <uuid>` = deterministyczny klucz
   (UUIDv5 z Bebok session id + licznik generacji); upraszcza krok 3.

## Unieważnianie

- `store/lifecycle.rs`: fork, truncate, compact, zmiana modelu/providera
  → `cli_session = None`. Fork może zamiast tego użyć `--resume key --fork-session`
  **[verify]**, ale tylko jeśli fork jest na końcu historii.
- Zmiana `system_hash` lub `profile_hash` → pełny transkrypt.

## Fallback

- stderr/`result` z „No conversation found” → `CliError::ResumeLost`
  (nowy wariant, `is_transient() == false`) → `CliProvider` jednorazowo
  ponawia z pełnym transkryptem i czyści klucz.

## Testy

- `CliProvider` z fałszywym backendem: druga tura wysyła tylko nowe wiadomości.
- lifecycle: truncate czyści `cli_session`.
- `ResumeLost` → retry z pełnym transkryptem, dokładnie raz.

## Pliki

`bebok-core/src/{session/persist.rs,store/lifecycle.rs,provider.rs}`,
`bebok-server/src/services/turn.rs`,
`bebok-cli-agents/src/{backend.rs,provider.rs,backends/claude.rs,transcript.rs}`.
