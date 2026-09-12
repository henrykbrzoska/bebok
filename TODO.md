# Dalsze prace

- [ ] Jako następny krok ustabilizować CI: przejrzeć historyczne nieudane runy na Windows i Ubuntu, odróżnić błędy środowiska od regresji oraz dodać powtarzalny proces weryfikacji buildów Tauri i artefaktów wydania. Obecne CI sprawdza silnik i klienta, ale nie buduje instalatorów.
- [ ] Rozszerzyć animacje logo na pozostałe stany pracy: łączenie z silnikiem, wywołanie narzędzia, uruchomiony terminal i ładowanie czatu. Stan „Bebok myśli” ma już małe animowane logo; kolejne warianty również muszą respektować `prefers-reduced-motion` i przekazywać stan tekstem.
- [ ] Wykonać ręczny test wyboru katalogu w przeglądarce i Tauri na Linuksie (GTK/KDE). Implementacja używa pickerów właściwych dla platformy i przechodzi build, ale w tym środowisku nie było dostępnej sesji linuksowego pulpitu.
