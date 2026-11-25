<p align="center"><b>English</b> · <a href="README.de.md">Deutsch</a></p>

<p align="center"><img src="assets/icon/tast2controller.svg" width="112" alt="Tast2Controller logo"></p>

<h1 align="center">Tast2ControllerRust</h1>

<p align="center">
  <a href="https://github.com/Niklas970/Tast2ControllerRust/releases/latest"><img src="https://img.shields.io/github/v/release/Niklas970/Tast2ControllerRust?label=Download" alt="Latest version"></a>
  <a href="https://github.com/Niklas970/Tast2ControllerRust/actions/workflows/ci.yml"><img src="https://github.com/Niklas970/Tast2ControllerRust/actions/workflows/ci.yml/badge.svg" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-blue" alt="AGPL-3.0 license"></a>
  <img src="https://img.shields.io/badge/platform-Windows%2010%20%7C%2011-0078d4" alt="Windows 10 | 11">
  <img src="https://img.shields.io/badge/status-alpha-e2793f" alt="Alpha">
  <img src="https://img.shields.io/badge/Rust-stable-dea584?logo=rust&logoColor=white" alt="Rust">
</p>

<p align="center">
  Mouse and keyboard as a DualSense for the PS5, straight over Remote Play on your own network.<br>
  Input only, no video and no sound: you play on the TV, the PC is just the controller.
</p>

<p align="center"><b><a href="https://github.com/Niklas970/Tast2ControllerRust/releases/latest">⬇ Download for Windows</a></b> · <a href="#installation">Installation</a> · <a href="#getting-started">Getting started</a></p>

> **Alpha.** It builds and the tests pass, but it has hardly been tried on a real PS5 yet. Please report problems as an [issue](https://github.com/Niklas970/Tast2ControllerRust/issues).

> **Story and offline games only.** Mouse and keyboard can break the rules of online games. Follow the rules of each game.

## What it does

- **One EXE, no setup.** Without arguments it opens the window, with arguments it is a command line tool.
- **Pairing without an IP.** It finds the PS5 on the network and finds it again after its IP changes. You sign in to Sony yourself in your browser; the program never sees your password.
- **Play in the window.** After "Connect", mouse and keyboard control the PS5 and the cursor stays inside the window. F8 releases them or takes them back, F12 disconnects.
- **Mouse as a stick, done well.** Curve, radial anti-deadzone, acceleration, smoothing and an ADS factor while aiming (L2), set per profile on the "Mouse" tab.
- **Profiles and templates.** Ready-made layouts for shooters, GTA (on foot and driving) and menus. Switch profiles with a key, edit bindings on the "Profiles & keys" tab.
- **Turn the PS5 on and off.** Wake it from rest mode, send it to rest mode and see its state, right in the window.
- **Tray.** Minimizing puts the window in the notification area. Right-click to connect, disconnect or quit; the tooltip shows the connection state. It can start in the tray and connect right away.
- **In-app updates.** The window reports new versions, downloads the EXE from GitHub, checks its SHA-256 and restarts.

## Installation

1. Download `tast2controller.exe` from [Releases](https://github.com/Niklas970/Tast2ControllerRust/releases/latest) and put it in a folder of its own.
2. The EXE is not signed, so Windows SmartScreen warns: "More info" → "Run anyway". The SHA-256 is in `tast2controller.exe.sha256`.
3. Start it. From then on it fetches new versions itself.

Requirements: Windows 10 or 11, the PS5 on the same network with Settings › System › Remote Play › "Enable Remote Play" on. To wake it from rest mode, also turn on "Enable Turning On PS5 from Network" under Power Saving.

## Getting started

The window is in German for now; the button names below are translations ("Verbinden" = Connect, "Steuerung übernehmen" = Take control, "Koppeln" = Pair).

1. **Pair** ("Connection" tab): "Open Sony sign-in", sign in in your browser, paste the address of the "redirect" page and press "Get account ID". Then read the code on the PS5 under Settings › System › Remote Play › Link Device, pick the PS5 that was found, enter the code and press "Pair".
2. **Connect:** press "Connect". If the PS5 is in rest mode, the program wakes it. Once connected, mouse and keyboard control the console.
3. **Release:** F8 gives mouse and keyboard back to Windows, F8 or "Take control" takes them again. F12 disconnects.
4. **Adjust:** pick a template or bind keys under "Profiles & keys", set sensitivity and curve under "Mouse". Changes are saved and apply from the next connection.

### Default layout (shooter)

| Key | PS5 | Key | PS5 |
|---|---|---|---|
| W A S D | left stick | Mouse | right stick |
| Left click | R2 (fire) | Right click | L2 (aim) |
| Space | Cross | C | Circle |
| R / F | Square | 1 / wheel | Triangle |
| Q | L1 | E / G | R1 |
| Shift | L3 | V / wheel click | R3 |
| Ctrl | walk slowly | Arrow keys | D-pad |
| Tab | Touchpad | Esc | Options |
| Backspace | Create | Home | PS button |
| F2 | profile "menu" | F8 / F12 | release / disconnect |

### Command line

```
tast2controller register        pair with the PS5
tast2controller run             connect without the settings window
tast2controller discover        search the network for a PS5
tast2controller wake | sleep    wake the PS5 or send it to rest mode
tast2controller status          on or in rest mode?
tast2controller keys            key names for the config
tast2controller update [--check]
```

The config is `tast2controller.toml` next to the EXE or in `%APPDATA%\tast2controller`. A commented example is in [`crates/app/config.example.toml`](crates/app/config.example.toml).

## Privacy

No account, no telemetry, no ads. Network traffic happens only when you start it:

- The Sony sign-in runs in your browser. Afterwards the program only exchanges the code from the address for your account ID.
- Pairing, connecting, waking and searching talk directly to the PS5 on your local network.
- The update check asks GitHub for the latest version and downloads the EXE only when you update.

The keys from pairing are stored only in your local config. Don't share them.

## Limits

- Input only. Video and sound still go from the PS5 to the TV. The console sends a video stream anyway; the program asks for the smallest one and discards it.
- The mouse is turned into stick movement. That feels better than keys alone, but not like a real mouse on a PC.
- Gyro aiming as a mouse target is experimental.
- One PS5 and one connection at a time.

## Development

Rust for protocol, input and interface ([Rune](https://github.com/anomalyco/rune), with its own Rust version in `gui/`).

| Path | Contents |
|---|---|
| `crates/core` | Remote Play protocol, pairing, controller state (platform independent, tested on Linux) |
| `crates/input` | Mouse engine: curve, radial anti-deadzone, ADS, smoothing |
| `crates/app` | Config, key mapping, templates, PSN sign-in, Windows input (Raw Input), tray, updater, command line |
| `gui` | The `tast2controller` program: Rune window, its own workspace |

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cd gui && cargo build --release
```

New version: raise the version in `Cargo.toml` and `gui/Cargo.toml`, then start the "Release" workflow with the matching tag (e.g. `v0.2.1`). It stops if tag and version don't match.

The logo is in `assets/icon/` (SVG as the source, ICO for the EXE, PNG for the window and docs).

## License and origin

[AGPL-3.0-only](LICENSE). The protocol core comes from [chiaki-controller](https://github.com/Acidream/chiaki-controller) and is based on chiaki-ng, see [`NOTICE`](NOTICE). Not affiliated with Sony. PlayStation, PS5 and DualSense are trademarks of Sony Interactive Entertainment.
