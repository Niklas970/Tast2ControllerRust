<p align="center"><a href="README.md">English</a> · <b>Deutsch</b></p>

<p align="center"><img src="assets/icon/tast2controller.svg" width="112" alt="Tast2Controller-Logo"></p>

<h1 align="center">Tast2ControllerRust</h1>

<p align="center">
  <a href="https://github.com/Niklas970/Tast2ControllerRust/releases/latest"><img src="https://img.shields.io/github/v/release/Niklas970/Tast2ControllerRust?label=Download" alt="Neueste Version"></a>
  <a href="https://github.com/Niklas970/Tast2ControllerRust/actions/workflows/ci.yml"><img src="https://github.com/Niklas970/Tast2ControllerRust/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-blue" alt="Lizenz AGPL-3.0"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078d4" alt="Windows 10 | 11">
  <img src="https://img.shields.io/badge/status-alpha-e2793f" alt="Alpha">
  <img src="https://img.shields.io/badge/Rust-stable-dea584?logo=rust&logoColor=white" alt="Rust">
</p>

<p align="center">
  Maus und Tastatur als DualSense für die PS5, direkt über Remote Play im eigenen Netz.<br>
  Nur Eingabe, kein Video und kein Ton: du spielst am Fernseher, der PC ist nur der Controller.
</p>

<p align="center"><b><a href="https://github.com/Niklas970/Tast2ControllerRust/releases/latest">⬇ Download für Windows</a></b> · <a href="#installation">Installation</a> · <a href="#erste-schritte">Erste Schritte</a></p>

> **Alpha.** Das Programm baut und die Tests laufen, an einer echten PS5 ist es aber noch kaum erprobt. Fehler bitte als [Issue](https://github.com/Niklas970/Tast2ControllerRust/issues) melden.

> **Nur für Story- und Offline-Spiele.** Maus und Tastatur können in Online-Spielen gegen deren Regeln verstoßen. Beachte die Regeln des jeweiligen Spiels.

## Was es kann

- **Eine EXE, kein Setup.** Ohne Argumente öffnet sich das Fenster, mit Argumenten ist es ein Kommandozeilen-Werkzeug.
- **Koppeln ohne IP.** Das Programm findet die PS5 im Netz und findet sie nach einem IP-Wechsel wieder. Die Anmeldung bei Sony machst du selbst im Browser, dein Passwort sieht das Programm nie.
- **Steuern im Fenster.** Nach „Verbinden“ steuern Maus und Tastatur die PS5, der Mauszeiger bleibt im Fenster. F8 gibt beides frei oder holt es zurück, F12 trennt.
- **Maus wie ein Stick, nur besser.** Kurve, radiale Anti-Deadzone, Beschleunigung, Glättung und ein ADS-Faktor beim Zielen (L2). Alles pro Profil im Tab „Maus“ einstellbar.
- **Profile und Vorlagen.** Fertige Belegungen für Shooter, GTA (zu Fuß und im Fahrzeug) und Menüs. Profile wechselst du per Taste, Belegungen änderst du im Tab „Profile & Tasten“.
- **PS5 ein- und ausschalten.** Aus dem Ruhemodus wecken, in den Ruhemodus schicken und den Zustand anzeigen, direkt im Fenster.
- **Tray.** Minimieren legt das Fenster in den Infobereich. Dort per Rechtsklick verbinden, trennen oder beenden, der Tooltip zeigt den Verbindungsstatus. Auf Wunsch startet es im Tray und verbindet sich gleich.
- **Updates im Programm.** Das Fenster meldet neue Versionen, lädt die EXE von GitHub, prüft die SHA-256 und startet neu.

## Installation

1. `tast2controller.exe` aus den [Releases](https://github.com/Niklas970/Tast2ControllerRust/releases/latest) laden und in einen eigenen Ordner legen.
2. Die EXE ist nicht signiert, Windows SmartScreen warnt daher: „Weitere Informationen“ → „Trotzdem ausführen“. Die SHA-256 steht in `tast2controller.exe.sha256`.
3. Starten. Weitere Versionen holt das Programm danach selbst.

Voraussetzung: Windows 10 oder 11, PS5 im selben Netz, unter Einstellungen › System › Remote-Wiedergabe „Remote-Wiedergabe aktivieren“ an. Zum Wecken aus dem Ruhemodus außerdem unter Energiesparen „Einschalten der PS5 über das Netzwerk“.

## Erste Schritte

1. **Koppeln** (Tab „Verbindung“): „Sony-Anmeldung öffnen“, im Browser anmelden, die Adresse der Seite „redirect“ einfügen und „Account-ID holen“. Dann an der PS5 unter Einstellungen › System › Remote-Wiedergabe › Gerät verknüpfen den Code ablesen, die gefundene PS5 wählen, Code eintragen und „Koppeln“.
2. **Verbinden:** „Verbinden“ drücken. Ist die PS5 im Ruhemodus, weckt das Programm sie. Sobald die Verbindung steht, steuern Maus und Tastatur die Konsole.
3. **Freigeben:** F8 gibt Maus und Tastatur zurück an Windows, F8 oder „Steuerung übernehmen“ holt sie wieder. F12 trennt.
4. **Anpassen:** Unter „Profile & Tasten“ eine Vorlage wählen oder Tasten belegen, unter „Maus“ Empfindlichkeit und Kurve einstellen. Änderungen werden gespeichert und gelten ab der nächsten Verbindung.

### Standardbelegung (Shooter)

| Taste | PS5 | Taste | PS5 |
|---|---|---|---|
| W A S D | linker Stick | Maus | rechter Stick |
| Linksklick | R2 (feuern) | Rechtsklick | L2 (zielen) |
| Leertaste | Kreuz | C | Kreis |
| R / F | Quadrat | 1 / Mausrad | Dreieck |
| Q | L1 | E / G | R1 |
| Umschalt | L3 | V / Mausrad-Klick | R3 |
| Strg | langsam gehen | Pfeiltasten | Steuerkreuz |
| Tab | Touchpad | Esc | Options |
| Rücktaste | Create | Pos1 | PS-Taste |
| F2 | Profil „menu“ | F8 / F12 | freigeben / trennen |

### Kommandozeile

```
tast2controller register        PS5 koppeln
tast2controller run             verbinden, ohne Einstellungsfenster
tast2controller discover        PS5 im Netz suchen
tast2controller wake | sleep    PS5 wecken oder in den Ruhemodus schicken
tast2controller status          an oder im Ruhemodus?
tast2controller keys            Tastennamen für die Config
tast2controller update [--check]
```

Die Config liegt in `tast2controller.toml` neben der EXE oder in `%APPDATA%\tast2controller`. Eine kommentierte Vorlage steht in [`crates/app/config.example.toml`](crates/app/config.example.toml).

## Datenschutz

Das Programm hat kein Konto, keine Telemetrie und keine Werbung. Netzverkehr gibt es nur, wenn du ihn auslöst:

- Die Sony-Anmeldung läuft in deinem Browser. Das Programm tauscht danach nur den Code aus der Adresse gegen deine Account-ID.
- Koppeln, Verbinden, Wecken und Suchen gehen direkt an die PS5 im lokalen Netz.
- Die Update-Prüfung fragt GitHub nach der neuesten Version und lädt die EXE nur, wenn du aktualisierst.

Die Schlüssel aus dem Koppeln stehen nur in deiner lokalen Config. Gib sie nicht weiter.

## Grenzen

- Nur Eingabe. Video und Ton kommen weiter über die PS5 an den Fernseher. Die Konsole schickt trotzdem einen Videostrom, das Programm fordert den kleinsten an und verwirft ihn.
- Die Maus wird in Stickbewegung umgerechnet. Das fühlt sich besser an als reine Tasten, aber nicht wie eine echte Maus am PC.
- Gyro-Zielen als Maus-Ziel ist experimentell.
- Nur eine PS5 und eine Verbindung gleichzeitig.

## Entwicklung

Rust für Protokoll, Eingabe und Oberfläche ([Rune](https://github.com/anomalyco/rune), eigene Rust-Version in `gui/`).

| Pfad | Inhalt |
|---|---|
| `crates/core` | Remote-Play-Protokoll, Registrierung, Controller-Zustand (plattformunabhängig, unter Linux testbar) |
| `crates/input` | Maus-Engine: Kurve, radiale Anti-Deadzone, ADS, Glättung |
| `crates/app` | Config, Tastenbelegung, Vorlagen, PSN-Login, Windows-Eingabe (Raw Input), Tray, Updater, Kommandozeile |
| `gui` | Das Programm `tast2controller`: Rune-Fenster, eigener Workspace |

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cd gui && cargo build --release
```

Neue Version: Version in `Cargo.toml` und `gui/Cargo.toml` erhöhen, dann den Workflow „Release“ mit dem passenden Tag (z. B. `v0.2.1`) starten. Er bricht ab, wenn Tag und Version nicht zusammenpassen.

Das Logo liegt unter `assets/icon/` (SVG als Quelle, ICO für die EXE, PNG für Fenster und Doku).

## Lizenz und Herkunft

[AGPL-3.0-only](LICENSE). Der Protokoll-Kern stammt aus [chiaki-controller](https://github.com/Acidream/chiaki-controller) und basiert auf chiaki-ng, siehe [`NOTICE`](NOTICE). Nicht mit Sony verbunden. PlayStation, PS5 und DualSense sind Marken von Sony Interactive Entertainment.
