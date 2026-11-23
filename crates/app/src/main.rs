//! tast2controller: an input-only PS5 Remote Play client for Windows.
//! Sends keyboard and mouse input to a PS5 as a DualSense; never decodes video or audio.

// Several APIs are only used by the settings window, which is not built yet.
#![allow(dead_code)]

mod config;
mod input;
mod psn;
#[cfg(windows)]
mod runner;

pub use tast2controller_core::{controller, http, logbuf, regist, session, stream};

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use base64::Engine;
use clap::{Parser, Subcommand};

use config::Config;

#[derive(Parser)]
#[command(
    version,
    about = "Input-only PS5 Remote Play client: keyboard and mouse as a DualSense, no video or audio"
)]
struct Cli {
    /// Config file (default: tast2controller.toml in the current directory, next to the program,
    /// or in %APPDATA%\tast2controller)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    /// More logging (-v debug, -vv trace)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Write a default config file with a keyboard and mouse layout
    Init {
        /// Overwrite an existing file
        #[arg(long)]
        force: bool,
    },
    /// Pair with a PS5 (Settings > System > Remote Play > Link Device shows the code)
    Register {
        /// IP address of the PS5
        #[arg(long)]
        host: String,
        /// The 8 digit code shown on the console
        #[arg(long)]
        pin: String,
        /// Your PSN account id: base64 (as chiaki-ng uses it) or the decimal number
        #[arg(long)]
        account_id: String,
    },
    /// Open the settings window and tray icon (the default)
    Gui,
    /// Connect and send controller input, without the settings window
    Run,
    /// Look up your PSN account ID (sign in on Sony's page in your browser)
    AccountId,
    /// Wake the PS5 from rest mode
    Wake,
    /// Put the PS5 into rest mode
    Sleep,
    /// Show whether the PS5 is on or in rest mode
    Status,
    /// List the input names usable in the config
    Keys,
}

fn default_config_path() -> PathBuf {
    let name = "tast2controller.toml";
    let cwd = PathBuf::from(name);
    if cwd.exists() {
        return cwd;
    }
    if let Some(exe_dir) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
    {
        let p = exe_dir.join(name);
        if p.exists() {
            return p;
        }
    }
    match std::env::var_os("APPDATA") {
        Some(appdata) => PathBuf::from(appdata).join("tast2controller").join(name),
        None => cwd,
    }
}

/// Detaches from the console window when it was opened just for this process
/// (started from Explorer), so the tray app doesn't leave a console behind.
#[cfg(windows)]
fn detach_own_console() {
    use windows_sys::Win32::System::Console::{FreeConsole, GetConsoleProcessList};
    let mut pids = [0u32; 2];
    unsafe {
        if GetConsoleProcessList(pids.as_mut_ptr(), 2) == 1 {
            FreeConsole();
        }
    }
}

#[cfg(windows)]
fn run_session(cfg: &Config) -> Result<()> {
    runner::Runner::start(cfg, runner::CaptureIn::OwnWindow)?.wait()
}

#[cfg(not(windows))]
fn run_session(_cfg: &Config) -> Result<()> {
    bail!("capturing keyboard and mouse is only implemented for Windows")
}

pub fn resolve_host(host: &str) -> Result<std::net::IpAddr> {
    use std::net::ToSocketAddrs;
    if let Ok(ip) = host.trim().parse() {
        return Ok(ip);
    }
    (host.trim(), 0)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}"))?
        .find(|a| a.is_ipv4())
        .map(|a| a.ip())
        .with_context(|| format!("no IPv4 address for {host}"))
}

pub fn parse_account_id(s: &str) -> Result<[u8; 8]> {
    if let Ok(n) = s.trim().parse::<u64>() {
        return Ok(n.to_le_bytes());
    }
    let b = base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .context("account id is neither a number nor base64")?;
    b.try_into()
        .map_err(|_| anyhow::anyhow!("account id must decode to 8 bytes"))
}

/// Sets `key = "value"` inside the `[console]` table of a TOML document, keeping comments.
fn set_console_value(doc: &str, key: &str, value: &str) -> String {
    let line_new = format!("{key} = \"{value}\"");
    let mut out = Vec::new();
    let mut in_console = false;
    let mut done = false;
    let mut console_header_index = None;
    for line in doc.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            if in_console && !done {
                // key missing in [console]: append before the next table
                out.push(line_new.clone());
                done = true;
            }
            in_console = t == "[console]";
            if in_console {
                console_header_index = Some(out.len());
            }
        } else if in_console && !done {
            let k = t.trim_start_matches('#').trim();
            if k.split('=').next().map(str::trim) == Some(key)
                && (t.starts_with(key) || t.starts_with('#'))
            {
                out.push(line_new.clone());
                done = true;
                continue;
            }
        }
        out.push(line.to_string());
    }
    if !done {
        if in_console {
            out.push(line_new);
        } else if console_header_index.is_none() {
            out.insert(0, String::new());
            out.insert(0, line_new);
            out.insert(0, "[console]".to_string());
        }
    }
    let mut s = out.join("\n");
    s.push('\n');
    s
}

fn main() {
    let cli = Cli::parse();
    let level = match cli.verbose {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(level))
        .format_timestamp_millis()
        .target(env_logger::Target::Pipe(Box::new(logbuf::Tee)))
        .init();
    if let Err(e) = real_main(cli) {
        log::error!("{e:#}");
        std::process::exit(1);
    }
}

fn real_main(cli: Cli) -> Result<()> {
    let config_path = cli.config.clone().unwrap_or_else(default_config_path);
    match cli.command.unwrap_or(Command::Gui) {
        Command::Gui => {
            #[cfg(windows)]
            detach_own_console();
            bail!("the settings window is not built yet; use `run`")
        }
        Command::Init { force } => {
            if config_path.exists() && !force {
                bail!(
                    "{} already exists (use --force to overwrite)",
                    config_path.display()
                );
            }
            std::fs::write(&config_path, config::DEFAULT_CONFIG)?;
            println!("wrote {}", config_path.display());
            Ok(())
        }
        Command::Register {
            host,
            pin,
            account_id,
        } => {
            let ip = resolve_host(&host)?;
            let pin: u32 = pin
                .trim()
                .parse()
                .context("the code must be the 8 digits shown on the console")?;
            let account = parse_account_id(&account_id)?;
            log::info!("registering with {ip} ...");
            let reg = regist::register(ip, pin, account)?;
            let mut doc = if config_path.exists() {
                std::fs::read_to_string(&config_path)?
            } else {
                config::DEFAULT_CONFIG.to_string()
            };
            doc = set_console_value(&doc, "host", &host);
            doc = set_console_value(
                &doc,
                "regist_key",
                &http::hex(reg.creds.regist_key_trimmed()),
            );
            doc = set_console_value(&doc, "rp_key", &http::hex(&reg.creds.morning));
            if !reg.nickname.is_empty() {
                doc = set_console_value(&doc, "nickname", &reg.nickname);
            }
            toml::from_str::<Config>(&doc).context("updated config does not parse")?;
            std::fs::write(&config_path, doc)?;
            println!(
                "registered with {} (MAC {}); credentials saved to {}",
                if reg.nickname.is_empty() {
                    "console"
                } else {
                    &reg.nickname
                },
                reg.mac,
                config_path.display()
            );
            Ok(())
        }
        Command::AccountId => {
            println!("1. Sign in on Sony's page (opening it in your browser now). Your password goes only to Sony.");
            println!(
                "   If it doesn't open, copy this address into your browser:
   {}
",
                psn::LOGIN_URL
            );
            if let Err(e) = psn::open_login_page() {
                log::warn!("{e:#}");
            }
            println!("2. After signing in, the page says \"redirect\". Copy the address from the address bar,");
            println!("   paste it here and press Enter:");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            let code = psn::code_from_redirect(&line)?;
            let id = psn::account_id_from_code(&code)?;
            println!(
                "
Your PSN account ID: {}   (as a number: {})",
                id.base64, id.number
            );
            println!("Use it with: tast2controller register --host <PS5 address> --pin <code> --account-id {}", id.base64);
            Ok(())
        }
        Command::Keys => {
            println!("Keyboard: A-Z, 0-9, F1-F24, Numpad0-Numpad9, and:");
            let names = [
                "Space",
                "Enter",
                "NumpadEnter",
                "Escape",
                "Tab",
                "Backspace",
                "CapsLock",
                "LShift",
                "RShift",
                "LCtrl",
                "RCtrl",
                "LAlt",
                "RAlt",
                "LWin",
                "RWin",
                "Menu",
                "Up",
                "Down",
                "Left",
                "Right",
                "Insert",
                "Delete",
                "Home",
                "End",
                "PageUp",
                "PageDown",
                "PrintScreen",
                "ScrollLock",
                "Pause",
                "NumLock",
                "NumpadMultiply",
                "NumpadAdd",
                "NumpadSubtract",
                "NumpadDecimal",
                "NumpadDivide",
                "Minus",
                "Equals",
                "LBracket",
                "RBracket",
                "Backslash",
                "Semicolon",
                "Quote",
                "Comma",
                "Period",
                "Slash",
                "Grave",
            ];
            println!("  {}", names.join(", "));
            println!("Mouse: MouseLeft, MouseRight, MouseMiddle, Mouse4, Mouse5, WheelUp, WheelDown, WheelLeft, WheelRight");
            println!("The window also shows the name of the last input pressed.");
            Ok(())
        }
        cmd => {
            let cfg = Config::load(&config_path)?;
            let creds = cfg.credentials()?;
            if cfg.console.host.is_empty() {
                bail!("console.host is not set in {}", config_path.display());
            }
            let ip = resolve_host(&cfg.console.host)?;
            match cmd {
                Command::Wake => {
                    regist::wakeup(ip, &creds)?;
                    println!("wakeup sent to {ip}");
                    Ok(())
                }
                Command::Status => {
                    println!("{}", regist::probe(ip)?);
                    Ok(())
                }
                Command::Sleep => {
                    let nonce = session::request_session(ip, &creds)?;
                    let ctrl = session::Ctrl::connect(
                        ip,
                        &nonce,
                        &creds,
                        cfg.console.login_pin.as_deref(),
                        false,
                    )?;
                    ctrl.goto_bed()?;
                    thread::sleep(Duration::from_millis(500));
                    println!("rest mode requested");
                    Ok(())
                }
                _ => run_session(&cfg),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses_and_maps() {
        let cfg: Config = toml::from_str(config::DEFAULT_CONFIG).unwrap();
        input::mapper::Mapper::new(&cfg).unwrap();
    }

    #[test]
    fn set_console_value_replaces_and_inserts() {
        let doc = config::DEFAULT_CONFIG;
        let d = set_console_value(doc, "host", "10.0.0.5");
        let d = set_console_value(&d, "regist_key", "abcd");
        let d = set_console_value(&d, "rp_key", "00112233445566778899aabbccddeeff");
        let d = set_console_value(&d, "nickname", "PS5-123");
        let cfg: Config = toml::from_str(&d).unwrap();
        assert_eq!(cfg.console.host, "10.0.0.5");
        assert_eq!(cfg.console.regist_key, "abcd");
        assert_eq!(cfg.console.nickname, "PS5-123");
        assert!(cfg.credentials().is_ok());
        // idempotent
        let d2 = set_console_value(&d, "host", "10.0.0.6");
        assert_eq!(d2.matches("host =").count(), 1);
    }

    #[test]
    fn account_id_formats() {
        let n: u64 = 1234567890123456789;
        let b64 = base64::engine::general_purpose::STANDARD.encode(n.to_le_bytes());
        assert_eq!(
            parse_account_id(&n.to_string()).unwrap(),
            parse_account_id(&b64).unwrap()
        );
    }
}
