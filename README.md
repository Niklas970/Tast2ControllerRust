<img src="assets/icon/tast2controller-128.png" width="96" alt="Tast2Controller-Logo" align="right">

# Tast2ControllerRust

Maus und Tastatur als DualSense für die PS5 (Remote Play, nur Eingabe, kein Video/Audio). Windows-Client in Rust mit verbesserter Maussteuerung.

**Status:** frühe Entwicklung. Protokoll-Kern und Maus-Engine laufen mit Tests unter Linux; Windows-Eingabe, Oberfläche (Rune) und Praxistest an der PS5 stehen noch aus.

## Aufbau
- `crates/core`: Remote-Play-Protokoll, Registrierung, Controller-Zustand
- `crates/input`: Maus-Engine (Kurve, radiale Anti-Deadzone, ADS, Glättung)
- `crates/app` (geplant): Windows-Eingabe, Tray, Rune-Oberfläche

## Updates
Beim Start prüft das Fenster, ob auf GitHub eine neuere Release liegt, und zeigt dann „Jetzt aktualisieren“. Das lädt die EXE, prüft sie gegen die veröffentlichte SHA-256, ersetzt die laufende Datei (die alte wird als `.old` beiseitegelegt und beim nächsten Start gelöscht) und startet neu. Auf der Kommandozeile:
```
tast2controller update --check
tast2controller update
```
Neue Release: Version in `Cargo.toml` und `gui/Cargo.toml` erhöhen, dann den Workflow „Release“ mit passendem Tag (z. B. `v0.2.0`) starten; der Workflow bricht ab, wenn Tag und Version nicht zusammenpassen.

## Entwickeln
```
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

## Logo
Eigener Entwurf (Controller mit klickendem Mauszeiger), unter `assets/icon/`: SVG als Quelle, ICO (16 bis 256 px) für die EXE, PNGs für Doku und Tray. `gui/build.rs` bettet das ICO beim Windows-Build in die EXE ein.

## Lizenz und Herkunft
AGPL-3.0-only. Der Protokoll-Kern stammt aus [chiaki-controller](https://github.com/Acidream/chiaki-controller) und basiert auf chiaki-ng, siehe `NOTICE`. Nicht mit Sony verbunden. Nur für die eigene PS5 im lokalen Netz und für Story-/Offline-Spiele gedacht.
