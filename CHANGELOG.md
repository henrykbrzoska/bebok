# Changelog

## 1.4.1 — 2026-09-12

- Lista sesji natychmiast pokazuje aktualny tytuł i użycie tokenów; przełączenie projektu czyści poprzednie sesje i odrzuca spóźnione odpowiedzi.
- Wiadomości oczekujące w kolejce zachowują agenta i model wybrane przy wysłaniu.
- Dostosowano dwa testy ścieżek do krótkiej postaci katalogu tymczasowego Windows; dodano trzy anglojęzyczne zrzuty aplikacji do README.

## 1.4.0 — 2026-09-12

- Poprawiono wybór katalogu projektu w aplikacji przeglądarkowej i Tauri, z uwzględnieniem ścieżek Windows i Linux oraz ponownego połączenia eksploratora po zmianie projektu.
- Ujednolicono wersje klienta, silnika i aplikacji Tauri. Aplikacja desktopowa uruchamia dołączony silnik jako sidecar na lokalnym porcie.
- Poprawiono obsługę modeli OpenAI: parametr limitu tokenów, wywołania narzędzi z trybem rozumowania, katalog modeli i obsługę błędów providera.
- Naprawiono ponawianie wiadomości, skrócono odstępy w czacie oraz zaktualizowano logo i ikonę aplikacji Windows.
- Wzmocniono ochronę konfiguracji projektu przy operacjach na plikach i poprawiono obsługę poleceń oraz terminala na Windows.

Pakiety Windows są budowane z `engine/` przez `cargo build --release`, a następnie z `client/` przez `npm run sidecar:copy` i `npm run tauri:build`. Instalatory NSIS/MSI, samodzielne pliki EXE i sumy SHA-256 są załączane do wydania GitHub.

Znane ograniczenie: wybór katalogu na pulpicie Linux wymaga jeszcze ręcznego testu w sesji GTK/KDE; kompilacja na Linux jest sprawdzana przez CI.
