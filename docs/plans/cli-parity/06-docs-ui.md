# Faza 6 — dokumentacja, ustawienia, changelog

- `docs/cli-agents.md`: tabela „Known limitations” odświeżana w każdym PR;
  nowe sekcje: profil (07), `extra.profile`, `extra.tools`, hooki, resume.
- `AGENTS.md` §4 „CLI providers” + §7 Gotchas: hooki jako nowa ścieżka
  uprawnień; gdzie leży wygenerowany profil; token turowy.
- `bebok-core/src/event.rs`: opisać nowe typy zdarzeń (jeśli powstaną,
  np. `cli.profile.changed`).
- `CHANGELOG.md` → `## Unreleased`: wpis dla użytkownika per faza.
- Settings → Providers (dla `kind: cli`):
  - wybór profilu: Bebok (zarządzany) / Mój Claude Code / Izolowany,
  - narzędzia: natywne CLI / Bebok / oba,
  - podgląd wygenerowanego profilu (read-only JSON) + „Otwórz katalog”,
  - wynik probe: wersja, wspierane flagi, status logowania.
  Nowe klucze i18n we wszystkich 12 słownikach, `data-testid` na kontrolkach.
- Pliki klienta: `client/src/views/settings/` (zakładka providers),
  `client/src/core/engine-client.service.ts` (`GET /cli/profile/{provider}`).
