# Tast2ControllerRust

Windows-Client, der Maus und Tastatur als DualSense an eine PS5 im lokalen Netz sendet (Remote-Play, nur Eingabe, kein Video/Audio).
Neuentwicklung, kein GitHub-Fork. Protokoll-Teile stammen aus chiaki-controller (AGPL-3.0-only), siehe NOTICE. Lizenz: AGPL-3.0-only.

## Aufbau (Cargo-Workspace, Vorbild: Faderust)
- `crates/core`: Remote-Play-Protokoll, Registrierung, Controller-Zustand (plattformunabhängig, unter Linux testbar)
- `crates/input`: Maus-Engine (Kurven, Anti-Deadzone, ADS, Glättung), reine Logik ohne Windows-Abhängigkeit
- `crates/app` (geplant): Oberfläche mit Rune wie in der Schmidhome-Desktop-App, Windows-Raw-Input, Tray

## Regeln
- Vor jedem Commit: `cargo fmt`, `cargo clippy -- -D warnings`, `cargo test`
- Keine Passwörter, Tokens oder Schlüssel in Code, Logs oder Doku; Zugangsdaten bleiben in der lokalen Config
- Neue Abhängigkeiten sparsam, Lizenz prüfen (AGPL-kompatibel)
- Antworten und Doku auf Deutsch, kurz; Code und Kommentare englisch
- Den Praxistest (PS5, GTA 6) macht Niklas am PC; ohne Test nichts als "funktioniert" melden
- Eingabe nur in Story-/Offline-Spielen empfehlen (Online-Regeln der Spiele beachten)
