# Faza 7 — zarządzany profil Claude Code („Bebok profile”)

Typ: `feat:`. Zależności: 01.1 (stdin + pliki tymczasowe).

## Idea

Dziś Bebok uruchamia `claude` z konfiguracją użytkownika (`~/.claude`,
jego settings, jego MCP, jego hooki, jego CLAUDE.md) i dokleja wszystko
przez argv. Wynik zależy od maszyny i jest niekontrolowany.

Zamiast tego Bebok **generuje profil** per instancja projektu: warstwę
ustawień, MCP, hooków, subagentów, skilli i system promptu, którą Claude
Code ładuje przez własne mechanizmy konfiguracji. „Narzut” Beboka (instrukcje,
polityki, reguły) przestaje być tekstem w prompcie, a staje się natywną
konfiguracją CLI, którą CLI faktycznie egzekwuje.

## Tryby (`providers[].extra.profile`)

| Tryb | Co ładuje CLI | Logowanie | Kiedy |
|---|---|---|---|
| `bebok` (domyślny) | profil Beboka **na wierzchu** ustawień użytkownika; `--setting-sources` ograniczone do `user` (bez `project/local` repo, które dubluje AGENTS.md) **[verify]**; `--strict-mcp-config` | konto użytkownika (bez zmian) | normalna praca |
| `user` | dzisiejsze zachowanie, profil tylko minimalny (hooki + mcp) | użytkownika | gdy ktoś chce swoich ustawień 1:1 |
| `isolated` | `CLAUDE_CONFIG_DIR=<profil>` — w pełni odcięty od `~/.claude` | **osobne** logowanie (credentials na Linux/Windows leżą w config dir; macOS keychain) **[verify]** | reprodukowalność, CI, schedules |

Ostrzeżenie w UI dla `isolated`: wymaga `claude login` z tym katalogiem;
Bebok może to uruchomić w terminalu PTY (`/terminal`) z ustawionym env.

## Gdzie leży profil

`<data_dir>/bebok/cli-profiles/<instance-hash>/<provider-name>/`:

```text
settings.json          # warstwa przekazywana przez --settings
mcp.json               # --mcp-config (most 04, MCP projektu na życzenie)
system.md              # --append-system-prompt-file (albo --system-prompt-file)
agents.json            # --agents (subagenci z presetów/fleet Beboka)
plugin/                # --plugin-dir [verify]
  .claude-plugin/plugin.json
  skills/<name>/SKILL.md   # skille Beboka (ten sam format SKILL.md)
  commands/*.md
  hooks/hooks.json         # albo hooki w settings.json
profile.lock.json      # hash wejść + wersja claude + lista użytych flag
```

Poza repo: nie ląduje w `git status`, nie wymaga `.gitignore`, sekrety
(token turowy) nigdy nie są zapisywane na stałe — token idzie przez env.

## Co Bebok wkłada do profilu (mapowanie konfiguracji)

| Bebok | Claude Code | Uwagi |
|---|---|---|
| AGENTS.md + skille (`bebok-skills`) | `plugin/skills/*` + `system.md` | skille ładowane leniwie przez CLI zamiast całości w prompcie |
| preset agenta (`code/ask/plan/debug`) | `system.md` + `permissions.defaultMode` | `ask`/`plan` → `plan` + deny mutujących |
| reguły `permission` (allow/ask/deny) | `permissions.allow/deny` w `settings.json` | tylko proste przekłady (`bash(git *)` → `Bash(git:*)`); reszta przez hook |
| `verify.buildTest` | `system.md` + hook `PreToolUse` (blokada test-runnerów przy `off`) | to samo co `build_test_gate.rs` |
| `yolo` | `permissions.defaultMode: acceptEdits` + allow listy | **nigdy** `bypassPermissions` |
| fleet / agenci plikowi | `agents.json` → `--agents` **[verify]** | CLI deleguje natywnie, Bebok widzi to jako Task tool |
| `model`, `thinking` | `--model`, `env.MAX_THINKING_TOKENS` **[verify]** | |
| — | `includeCoAuthoredBy: false` / `attribution` **[verify]** | AGENTS.md §6 zabrania trailerów atrybucji |
| — | `env.DISABLE_AUTOUPDATER=1`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, `DISABLE_TELEMETRY=1` **[verify]** | deterministyczne, ciche uruchomienia |
| `terminal`/`BEBOK_SHELL` | `env.SHELL` / `BASH_DEFAULT_TIMEOUT_MS` | |
| `context_budget` | `env.CLAUDE_CODE_AUTO_COMPACT_*` jeśli istnieje **[verify]** | inaczej tylko informacyjnie |

## Hooki — rdzeń tej fazy

`settings.json` (albo `plugin/hooks/hooks.json`):

```json
{
  "hooks": {
    "PreToolUse":  [{ "matcher": "*", "hooks": [{ "type": "command",
        "command": "\"<bebok-server>\" cli-hook pre",  "timeout": 600 }] }],
    "PostToolUse": [{ "matcher": "*", "hooks": [{ "type": "command",
        "command": "\"<bebok-server>\" cli-hook post", "timeout": 30 }] }],
    "Stop":        [{ "hooks": [{ "type": "command",
        "command": "\"<bebok-server>\" cli-hook stop", "timeout": 30 }] }]
  }
}
```

- `bebok-server cli-hook <event>` (nowa podkomenda w `bebok-server/src/cli.rs`):
  czyta JSON hooka ze stdin, `POST $BEBOK_HOOK_URL/<event>` z
  `Authorization: Bearer $BEBOK_HOOK_TOKEN`, wypisuje odpowiedź na stdout.
  Ten sam binarny, zero nowych zależności; ścieżka = `std::env::current_exe()`.
- Engine: `routes/cli_hook.rs`, `POST /cli-hook/{session}/{pre|post|stop}`
  (rejestracja w `routes/mod.rs`, token turowy).
  - `pre`: mapuje `tool_name`+`tool_input` na tekst reguły Beboka
    (`cli:Bash(npm test)`, `cli:Edit(src/x.rs)`), przepuszcza przez
    `build_test_gate` → permission engine → jev → **popup** (synchronicznie,
    hook czeka; stąd `timeout: 600`). Dla Edit/Write/MultiEdit/NotebookEdit:
    `Tracker::snapshot_before_write(path)`. Odpowiedź:
    `{"hookSpecificOutput":{"hookEventName":"PreToolUse",
      "permissionDecision":"allow|deny","permissionDecisionReason":"..."}}`
    **[verify kształt]**. „Always allow” → reguła projektu jak dziś.
  - `post`: rejestruje zmianę w `change_tracking`, publikuje
    `StreamEvent::ExternalTool` (05) z pełnym input/output.
  - `stop`: domyka turę (status, ewentualne podsumowanie zmian).
- Efekt: w trybie `native` CLI używa **swoich** narzędzi, ale każde
  przechodzi przez uprawnienia Beboka, popup, Changes panel i daje prawdziwy
  tool-row — bez refaktoru `exec.rs` z fazy 4.
- Fail-closed: hook nie dosięga silnika → `deny` z powodem (exit 2 / JSON
  deny), nigdy cichy allow.
- Wtedy `--permission-mode` może zostać `default`, a `allow_write` zmienia
  znaczenie: „czy w ogóle pytać o zapisy”, a nie „ślepo pozwól”.

## Zmiany w kodzie

1. `bebok-cli-agents/src/backend.rs`
   - `CliBackend::prepare(&self, spec, req, profile: &ProfileDir) -> PreparedTurn`
     gdzie `PreparedTurn { args_extra, env: Vec<(String,String)>, stdin, _tmp: Option<TempDir> }`.
   - `CliBackend::profile_writer(&self) -> Option<&dyn ProfileWriter>`.
2. `bebok-cli-agents/src/probe.rs` (nowy) — rozbudowa `probe_args`:
   `claude --version` + `claude --help` → `CliFeatures { version,
   flags: HashSet<String> }`, cache per (ścieżka, mtime binarnego).
   Każda opcjonalna flaga z tego planu jest bramkowana `features.has("--x")`.
   Wystawić w `POST /cli/backends/{id}/probe` (`routes/cli.rs`).
3. `bebok-cli-agents/src/profile.rs` (nowy) — `ProfileWriter`, generowanie
   plików, atomowy zapis (tmp+rename), `profile.lock.json` z hashem;
   regeneracja tylko przy zmianie hasha.
4. `bebok-cli-agents/src/backends/claude.rs` — `ClaudeProfileWriter`
   (mapowanie z tabeli), `build_command` dokłada `--settings`,
   `--mcp-config`, `--strict-mcp-config`, `--append-system-prompt-file`,
   `--agents`, `--plugin-dir` zależnie od trybu i probe.
5. `bebok-cli-agents/src/runner.rs` — `.env(...)` z `PreparedTurn`
   (`BEBOK_HOOK_URL`, `BEBOK_HOOK_TOKEN`, opcjonalnie `CLAUDE_CONFIG_DIR`);
   scrub env jak w PTY (`*API_KEY*` itp. poza tym, czego CLI potrzebuje
   — `ANTHROPIC_API_KEY` zostaje, jeśli użytkownik go ustawił).
   Platformowo: osobne `#[cfg] mod platform` (wzór już jest w runnerze).
6. `bebok-cli-agents/src/spec.rs` — nowe klucze `extra.profile`
   (`bebok|user|isolated`), `extra.tools` (04), `extra.profile_overrides`
   (obiekt deep-merge do `settings.json`), `extra.system_prompt_mode`
   (`append|replace`). Testy jak istniejące `from_extra`.
7. `bebok-core` — `TurnToken` (nowy moduł `cli_turn_token.rs` albo w
   `store/session_state.rs`): wydawany na start tury CLI, unieważniany na
   końcu/cancel, zakres = (session, turn). Wzór: tickety `bebok-pty`.
8. `bebok-core/src/store/instance_store.rs` — `reload_instance` unieważnia
   hash profilu (regeneracja przy następnej turze).
9. Warstwa projektu („własny narzut”): opcjonalne pliki w repo
   `<project>/.bebok/cli/claude/settings.json` (merge na koniec) i
   `<project>/.bebok/cli/claude/system.append.md` (doklejane do `system.md`).
   Kolejność: domyślne Beboka → global `extra.profile_overrides` →
   projektowe `extra.profile_overrides` → pliki `.bebok/cli/claude/*`.
   Klucze zakazane po merge (walidacja, błąd `CliError::Config`):
   `permissions.defaultMode = bypassPermissions`, usunięcie hooków Beboka
   w trybie `bebok`.
10. Route podglądu: `GET /cli/profile/{provider}?directory=` → wygenerowane
    pliki (bez tokenów) dla Settings (06).

## Testy

- `profile.rs`: golden pliki dla przykładowej konfiguracji; hash stabilny;
  zakazane klucze odrzucane.
- `spec.rs`: parsowanie nowych kluczy, domyślnie `bebok`.
- `claude.rs`: `build_command` z/bez flag w `CliFeatures`.
- `routes/cli_hook.rs`: pre → popup → allow/deny; brak tokenu → 401;
  token innej sesji → 403; snapshot przed Edit; post rejestruje zmianę.
- `cli-hook` podkomenda: silnik niedostępny → deny (fail-closed).
- `npm run check:windows` (ścieżki ze spacjami w `command` hooka, `.exe`).

## Ryzyka

- Format hooków / `--settings` / `--plugin-dir` / `--agents` zmienia się
  między wersjami CLI → twarde bramkowanie przez probe + test fixture
  per wersja; w `profile.lock.json` zapisana wersja, przy zmianie regeneracja.
- `timeout` hooka a popup: użytkownik odchodzi od komputera → hook timeout →
  CLI traktuje jako błąd hooka; ustawić Beboka na `deny` po
  `timeout - 5 s`, z jasnym powodem.
- `isolated` = drugie logowanie; nie może być domyślne.
- Hooki wykonują binarny Beboka z env — token turowy krótkożyjący,
  trasa przyjmuje tylko z `127.0.0.1`.
