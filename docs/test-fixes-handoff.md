# Handoff: naprawa testów klienta (49 failures) — stan na 2026-11-17

## Co się stało

Pełny przebieg `npm test --prefix client -- --watch=false`: **747 testów, 49 FAILED**
(log: `/tmp/karma-full.log`, powtórka `/tmp/karma-clean.log`).
Większość to mocki `EngineClient`/szablony, które nie nadążyły za serwisami —
część winy moich zmian (kroki 1–3 z `docs/editor-completion.md`), część zastana na HEAD.

## Już naprawione (grupy 1–10, pliki zmodyfikowane, niescommitowane)

1. `batch.store.spec.ts` — `subscribe` → `onEvent` (4 błędy).
2. `explorer.spec.ts` (F7-3) — dopisany mock `readLastDirectory` do mocka EngineClient.
3. `sidebar.spec.ts` + start-screen spec — `connection` to signal, nie funkcja;
   dopisane brakujące pola mocka (`connected`, `isDisconnected`, `disconnect`,
   `connect`, `ping`, `remoteDefaults`, `readLastDirectory`, `isFixedProfile`,
   `reconfigure`, `getProfile`, `saveFixedProfile`, `clearProfile`,
   `deleteSession`, `removeWorktree`, `create…`).
4. `new-session-dialog.html` — NG01352: dopisany brakujący `name` do inputu `agent-filter`.
5. `providers-tab.spec.ts` — dopisany provider `claude-cli` do mocka configu
   (test `select('claude-cli')`).
6. `tool-part.spec.ts` — 2 testy delegacji przepisane do rzeczywistej struktury
   szablonu (`.target-name` to `<a>` bez `.head-target`; toggle to `.tool-head`,
   nie `.head-toggle`).
7. `message-row.spec.ts` — klasa `collapsed` jest na `div.tool-head`, nie na buttonie.
8. `schedules.spec.ts` — `toggleForm()` czyści formularz tylko przy zamykaniu;
   test poprawiony na open → wpisz → close → assert pusto.
9. `code-features-card.spec.ts` — dopisane `fixture.detectChanges()` po każdym
   `toggle*`/`regenerate*` (efekt mirror w zoneless) + mock `getCodeMap`
   w teście regenerate.
10. `build-test-policy.spec.ts`, `frontend-verify.spec.ts` — dopisane
    `fixture.detectChanges()` po `selectMode('off')` (radio `[checked]` binduje
    `effectiveMode()`, DOM aktualizuje się dopiero przy CD).

## Zostało: terminal.spec.ts "patches a row on process.exited" (grupa 11)

**Objaw:** po wysłaniu eventu `process.exited` wiersz dalej `tone-running`,
brak tekstu `exit code 0` (`client/src/views/terminal/terminal.spec.ts:170-171`).

**Ustalenia z debugowania (logi `/tmp/karma-terminal*.log`):**
- `store.sessionId()` to `'s1'`, `listener` istnieje (`hasListener: true`),
  ale `store.processes()` dalej pokazuje `status: 'running'`.
- Tymczasowy `console.log('DBG handleEvent …')` w `processes.store.ts:268`
  **nigdy się nie drukuje** → `listener` w teście to NIE jest handler
  `ProcessesStore.handleEvent`, mimo że `terminal.ts` nie woła `onEvent`
  (sprawdzone grepem), podobnie `terminal-tab.ts` i `process-log-tab.ts`.
- Podejrzenie: coś innego subskrybuje mock `EventsStore.onEvent` PO storze
  i nadpisuje zmienną `listener` (mock zapisuje ostatnie `fn`).
  Kandydaci do sprawdzenia: `ChatSessionStore` (injectowany w teście przed
  komponentem), efekty `TerminalView` wołane w `ngOnInit`.

**Ślady debugowe DO POSPRZĄTANIA (zostawione celowo do dalszej diagnozy):**
- `client/src/core/processes.store.ts:268` — `console.log('DBG handleEvent exited', …)`
- `client/src/views/terminal/terminal.spec.ts:169` — `console.log('DBG store:', …)`

**Proponowany następny krok:** dodać `console.log` (ze stackiem) do mocka
`onEvent` w `terminal.spec.ts`, żeby zobaczyć KTO się subskrybuje i w jakiej
kolejności; potem zdecydować, czy naprawiać test (zbierać wszystkich
listenerów w tablicę i wołać właściwy), czy kod.

## Jak uruchamiać testy

- Całość: `npm test --prefix client -- --watch=false` (~5+ min, przekroczy limit
  pojedynczego wywołania — puszczać w tle: `background: true`, log do pliku).
- Jeden spec: `npm test --prefix client -- --watch=false --include='**/terminal.spec.ts'`
  (~100 s; też w tle z `ready_text: "EXIT:"` i `echo "EXIT:$?" >> log`).
- Karma loguje do stdout; `grep -a` na pliku logu (kolory ANSI).

## Uwagi

- `git status` pokazuje ~87 zmienionych plików — to cała robocza gałąź
  (engine kroki 1–3 + klient), nie tylko fixy testów. Fixy speców mieszać
  ostrożnie z resztą przy commicie.
- Nie zweryfikowano, czy 49 failures występuje też na czystym HEAD —
  jeśli po naprawie terminala coś jeszcze padnie w pełnym przebiegu,
  warto porównać z `git stash` dla podejrzanych plików.
