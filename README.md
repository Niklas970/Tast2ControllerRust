# Tast2ControllerRust

Maus und Tastatur als DualSense für die PS5 (Remote Play, nur Eingabe, kein Video/Audio). Windows-Client in Rust mit verbesserter Maussteuerung.

**Status:** frühe Entwicklung. Protokoll-Kern und Maus-Engine laufen mit Tests unter Linux; Windows-Eingabe, Oberfläche (Rune) und Praxistest an der PS5 stehen noch aus.

## Aufbau
- `crates/core`: Remote-Play-Protokoll, Registrierung, Controller-Zustand
- `crates/input`: Maus-Engine (Kurve, radiale Anti-Deadzone, ADS, Glättung)
- `crates/app` (geplant): Windows-Eingabe, Tray, Rune-Oberfläche

## Entwickeln
```
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

## Lizenz und Herkunft
AGPL-3.0-only. Der Protokoll-Kern stammt aus [chiaki-controller](https://github.com/Acidream/chiaki-controller) und basiert auf chiaki-ng, siehe `NOTICE`. Nicht mit Sony verbunden. Nur für die eigene PS5 im lokalen Netz und für Story-/Offline-Spiele gedacht.
