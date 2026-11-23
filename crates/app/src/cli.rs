//! Command line of the single `tast2controller` program (the GUI crate starts it when arguments are given).

#[cfg(windows)]
use crate::runner;
use crate::{
    config, http, logbuf, parse_account_id, psn, regist, resolve_host, session, set_console_value,
    update,
};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
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
    /// Download and install the newest release (checked against its SHA-256)
    Update {
        /// Only show whether a newer version exists
        #[arg(long)]
        check: bool,
    },
}

pub fn default_config_path() -> PathBuf {
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

/// Stores the result of a registration in the config file (created if missing), keeping comments.
pub fn save_registration(config_path: &Path, host: &str, reg: &regist::Registered) -> Result<()> {
    let mut doc = if config_path.exists() {
        std::fs::read_to_string(config_path)?
    } else {
        config::DEFAULT_CONFIG.to_string()
    };
    doc = set_console_value(&doc, "host", host);
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
    if let Some(dir) = config_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(config_path, doc)?;
    Ok(())
}

/// Detaches from the console window when it was opened just for this process
/// (started from Explorer), so the tray app doesn't leave a console behind.
#[cfg(windows)]
pub fn detach_own_console() {
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

pub fn main() {
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
    update::cleanup_old();
    let config_path = cli.config.clone().unwrap_or_else(default_config_path);
    match cli.command.unwrap_or(Command::Gui) {
        Command::Gui => {
            #[cfg(windows)]
            detach_own_console();
            bail!("start the program without arguments to open the settings window")
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
            save_registration(&config_path, &host, &reg)?;
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
        Command::Update { check } => {
            let current = update::current_version();
            let Some(rel) = update::check()? else {
                println!("tast2controller {current} is up to date");
                return Ok(());
            };
            println!("new version {} available (installed: {current})", rel.tag);
            if check {
                println!("install it with: tast2controller update");
                return Ok(());
            }
            println!("downloading and checking the SHA-256 ...");
            let path = update::install(&rel)?;
            println!(
                "updated {} to {}; start it again to use the new version",
                path.display(),
                rel.tag
            );
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
