# Release w 5 krokach

```bash
npm run release:check                     # gotowy? (read-only)
npm run release -- X.Y.Z                  # otwiera PR release/X.Y.Z
```

1. Zainstaluj draft z PR (niewidoczny dla userów), sprawdź *Check for updates*.
2. `gh pr merge <nr> --squash` → CI taguje i publikuje. Aplikacje same się zaktualizują.
3. `npm run release:status -- X.Y.Z` → gdzie jest wydanie.

## Awaria CI na PR

```bash
gh pr checks <nr>                         # co padło
gh run view <run-id> --job <job-id> --log | grep -ai -m5 "error"   # log kroku
```

Napraw → push na ten sam branch `release/X.Y.Z` → CI buduje draft od nowa.
Nie dotykaj assetów ręcznie — updater ufa tylko temu, co publikuje CI.

## Test pipeline bez tagowania

```bash
gh workflow run release.yml --ref main -f draft=true   # draft z main
gh run watch
gh release delete <tag> --yes             # posprzątaj testowy draft
```

## Zasady

- Wersja: `feat!` → major, `feat` → minor, reszta → patch. Pre-release tylko numeryczne (`X.Y.Z-1`, MSI odrzuca `-rc.1`).
- `## Unreleased` w CHANGELOG.md → notatki update w aplikacji. Pisz dla userów.
- Brak `.sig` = brak `TAURI_SIGNING_PRIVATE_KEY`. Klucz zgubiony = koniec auto-update.
- Szczegóły: `scripts/release.md` (mechanika CI), `CONTRIBUTING.md#releasing` (proces).
