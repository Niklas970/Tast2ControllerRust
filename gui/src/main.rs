//! tast2controller: keyboard and mouse as a DualSense for a PS5 (Remote Play, input only).
//! One program: without arguments it opens the settings window (Rune, like the Schmidhome
//! desktop app); with arguments it is the command line (`tast2controller --help`).

fn main() {
    if std::env::args().len() > 1 {
        tast2controller::cli::main();
        return;
    }
    #[cfg(windows)]
    {
        tast2controller::cli::detach_own_console();
        tast2controller::update::cleanup_old();
        ui::run();
    }
    #[cfg(not(windows))]
    eprintln!("the settings window is only available on Windows; try `tast2controller --help`");
}

#[cfg(windows)]
mod ui {
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::time::Duration;

    use rune::tok::{self, Theme, type_};
    use rune::{App, AppHandle, AppSpec, Color, Cx, El, TextEdit, col, row, text, text_input};
    use tast2controller::config::{Binding, Config, MouseTarget};
    use tast2controller::input::mapper::Mapper;
    use tast2controller::input::windows;
    use tast2controller::power::{self, Power};
    use tast2controller::{templates, tray};
    use tast2controller::runner::{CaptureIn, Runner};
    use tast2controller::regist::FoundConsole;
    use tast2controller::{DISCOVER_WAIT, cli, pick_for_pairing, psn, regist, resolve_host, update};
    use tast2controller_input::MouseConfig;

    static HANDLE: OnceLock<AppHandle<State>> = OnceLock::new();

    #[derive(Clone, Copy, PartialEq)]
    enum Tab {
        Connect,
        Keys,
        Mouse,
        Settings,
    }

    /// What the settings window knows about the paired PS5's power.
    #[derive(Clone, PartialEq)]
    enum PowerView {
        Unknown,
        On,
        Rest,
        Waking,
        Unreachable(String),
    }

    impl PowerView {
        fn label(&self) -> String {
            match self {
                PowerView::Unknown => "Zustand unbekannt".into(),
                PowerView::On => "Eingeschaltet".into(),
                PowerView::Rest => "Im Ruhemodus".into(),
                PowerView::Waking => "Wird eingeschaltet …".into(),
                PowerView::Unreachable(e) => format!("Nicht erreichbar: {e}"),
            }
        }
    }

    struct State {
        /// The config as last loaded or saved; the window saves by itself when `cfg` differs.
        saved: String,
        /// Whether the pairing card is open (always when not paired yet).
        pairing_open: bool,
        checking: bool,
        /// The message on screen and since when; it disappears after a few seconds.
        shown_message: (String, Option<std::time::Instant>),
        path: PathBuf,
        cfg: Config,
        runner: Option<Runner>,
        status: String,
        message: String,
        tab: Tab,
        /// Profile shown on the Keys and Mouse tabs.
        sel: usize,
        // pairing
        redirect: TextEdit,
        account: Option<u64>,
        /// Manual IP address, only needed when the search finds nothing.
        host: TextEdit,
        pin: TextEdit,
        busy: bool,
        /// Consoles found by the last network search.
        found: Vec<FoundConsole>,
        searching: bool,
        // new binding
        bind_input: TextEdit,
        bind_action: TextEdit,
        // self-update
        update: Option<update::Release>,
        updating: bool,
        /// Whether this window feeds the capture host (subclassed once, on first connect).
        capture_ready: bool,
        /// Whether keyboard and mouse go to the PS5 right now.
        capturing: bool,
        // power of the paired PS5
        power: PowerView,
        power_busy: bool,
        /// The settings window, once found (for the tray and for capturing).
        hwnd: Option<isize>,
        tray: Option<tray::Tray>,
    }

    impl Default for State {
        fn default() -> Self {
            let path = cli::default_config_path();
            let (cfg, message) = if path.exists() {
                match Config::load(&path) {
                    Ok(c) => (c, String::new()),
                    Err(e) => (Config::default_config(), format!("Config nicht geladen: {e:#}")),
                }
            } else {
                (Config::default_config(), "Noch keine Config: zuerst die PS5 koppeln.".into())
            };
            let host = TextEdit::new(cfg.console.host.clone());
            State {
                saved: cfg.to_text().unwrap_or_default(),
                pairing_open: false,
                checking: false,
                shown_message: (String::new(), None),
                path,
                cfg,
                runner: None,
                status: "Nicht verbunden".into(),
                message,
                tab: Tab::Connect,
                sel: 0,
                redirect: TextEdit::new(""),
                account: None,
                host,
                pin: TextEdit::new(""),
                busy: false,
                found: Vec::new(),
                searching: false,
                bind_input: TextEdit::new(""),
                bind_action: TextEdit::new(""),
                update: None,
                updating: false,
                capture_ready: false,
                capturing: false,
                power: PowerView::Unknown,
                power_busy: false,
                hwnd: None,
                tray: None,
            }
        }
    }

    fn accent() -> Color {
        rune::oklch(0.68, 0.16, 255.0)
    }

    fn button(label: &str, key: &str, t: Theme, primary: bool, on: impl Fn(&mut State) + 'static) -> El {
        let fill = if primary { accent() } else { t.card };
        let fg = if primary { Color::WHITE } else { t.text };
        row().key(key.to_string()).h(34.0).px(16.0).radius(9.0).center_y().fill(fill).border(1.0, t.card_border)
            .child(text(label.to_string()).font(type_::LABEL).color(fg))
            .on_press(on)
    }

    fn chip(label: &str, key: String, on_state: bool, t: Theme, on: impl Fn(&mut State) + 'static) -> El {
        row().key(key).h(30.0).px(14.0).radius(8.0).center_y()
            .fill(if on_state { accent() } else { t.card }).border(1.0, t.card_border)
            .child(text(label.to_string()).font(type_::LABEL).color(if on_state { Color::WHITE } else { t.text }))
            .on_press(on)
    }

    fn card(key: &str, t: Theme) -> El {
        col().key(key.to_string()).gap(10.0).padding(16.0).radius(12.0).fill(t.card).border(1.0, t.card_border)
    }

    fn field(key: &'static str, value: &TextEdit, hint: &str, t: Theme, f: fn(&mut State) -> &mut TextEdit) -> El {
        row().h(38.0).px(12.0).center_y().radius(10.0).fill(t.control_off).grow(1.0)
            .child(text_input(value, type_::BODY).key(key).single_line().grow(1.0).color(t.text)
                .placeholder(hint, t.text_secondary).on_edit(f))
    }

    fn sel_index(s: &State) -> usize {
        s.sel.min(s.cfg.profiles.len().saturating_sub(1))
    }

    fn nudge(s: &mut State, f: impl Fn(&mut MouseConfig)) {
        let i = sel_index(s);
        if let Some(p) = s.cfg.profiles.get_mut(i) {
            f(&mut p.mouse.engine);
            p.mouse.engine = p.mouse.engine.clone().sanitized();
        }
    }

    /// Hides a message 6 seconds after it appeared.
    fn fade_message(s: &mut State) {
        if s.message.is_empty() {
            s.shown_message = (String::new(), None);
        } else if s.shown_message.0 != s.message {
            s.shown_message = (s.message.clone(), Some(std::time::Instant::now()));
        } else if s.shown_message.1.is_some_and(|t| t.elapsed() > Duration::from_secs(6)) {
            s.message.clear();
        }
    }

    /// Saves the config when it changed since the last save (called twice a second).
    fn autosave(s: &mut State) {
        let Ok(text) = s.cfg.to_text() else { return };
        if text == s.saved {
            return;
        }
        match s.cfg.save(&s.path) {
            Ok(()) => s.saved = text,
            Err(e) => {
                s.saved = text; // don't retry every half second; the next change tries again
                s.message = format!("Speichern fehlgeschlagen: {e:#}");
            }
        }
    }

    fn start(s: &mut State) {
        if s.runner.is_some() {
            return;
        }
        if let Err(e) = s.cfg.credentials() {
            s.message = format!("{e:#}");
            return;
        }
        // Capture in this window; fall back to the small capture window if that fails.
        let capture_in = match capture_target(s) {
            Ok(hwnd) => CaptureIn::Window(hwnd),
            Err(e) => {
                log::warn!("capturing in the settings window failed, using an own window: {e:#}");
                CaptureIn::OwnWindow
            }
        };
        match Runner::start(&s.cfg, capture_in) {
            Ok(r) => {
                s.runner = Some(r);
                s.message = format!(
                    "Verbindung wird aufgebaut. {} gibt Maus und Tastatur frei oder holt sie zurück, {} trennt.",
                    s.cfg.input.capture_toggle, s.cfg.input.quit
                );
            }
            Err(e) => s.message = format!("Start fehlgeschlagen: {e:#}"),
        }
    }

    /// This window's handle, prepared for capturing. Runs on the UI thread, which owns the window.
    fn capture_target(s: &mut State) -> anyhow::Result<isize> {
        let hwnd = s.hwnd.or_else(windows::window_of_current_thread)
            .ok_or_else(|| anyhow::anyhow!("settings window not found"))?;
        if !s.capture_ready {
            windows::install_in_window(hwnd)?;
            s.capture_ready = true;
        }
        Ok(hwnd)
    }

    /// Finds the settings window, makes minimizing hide it to the tray and shows the tray icon.
    /// Runs on the UI thread; retries a few times while the window isn't visible yet.
    fn init_window(s: &mut State, tries: u32) {
        let Some(hwnd) = windows::window_of_current_thread() else {
            if tries > 0 {
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(300));
                    later(move |s| init_window(s, tries - 1));
                });
            }
            return;
        };
        s.hwnd = Some(hwnd);
        if let Err(e) = tray::hide_on_minimize(hwnd) {
            log::warn!("minimize to tray: {e:#}");
        }
        match tray::Tray::spawn("Tast2ControllerRust: nicht verbunden", on_tray) {
            Ok(t) => s.tray = Some(t),
            Err(e) => log::warn!("tray icon: {e:#}"),
        }
        if s.cfg.ui.start_in_tray && s.tray.is_some() {
            tray::hide_window(hwnd);
        }
        if s.cfg.ui.connect_on_launch && s.cfg.credentials().is_ok() {
            start(s);
        }
    }

    fn on_tray(ev: tray::TrayEvent) {
        match ev {
            tray::TrayEvent::Open => later(|s| {
                if let Some(h) = s.hwnd {
                    tray::show_window(h);
                }
            }),
            tray::TrayEvent::ToggleConnection => later(|s| {
                if s.runner.is_some() {
                    stop(s);
                } else {
                    start(s);
                }
            }),
            tray::TrayEvent::Quit => later(|s| {
                stop(s);
                s.tray = None;
                std::process::exit(0);
            }),
        }
    }

    fn take_input(s: &mut State) {
        if let Some(r) = &s.runner {
            r.set_capture(true);
        }
    }

    fn stop(s: &mut State) {
        if let Some(r) = s.runner.take() {
            r.request_stop();
            let _ = r.stop_and_wait();
        }
        s.status = "Nicht verbunden".into();
        s.capturing = false;
    }

    fn later(f: impl FnOnce(&mut State) + Send + 'static) {
        if let Some(h) = HANDLE.get() {
            h.update(move |s, _| f(s));
        }
    }

    fn open_login(s: &mut State) {
        match psn::open_login_page() {
            Ok(()) => s.message = "Melde dich im Browser bei Sony an, kopiere danach die Adresse der Seite „redirect“.".into(),
            Err(e) => s.message = format!("{e:#}"),
        }
    }

    fn fetch_account(s: &mut State) {
        if s.busy {
            return;
        }
        let code = match psn::code_from_redirect(&s.redirect.text) {
            Ok(c) => c,
            Err(e) => {
                s.message = format!("{e:#}");
                return;
            }
        };
        s.busy = true;
        s.message = "Frage Sony nach der Account-ID …".into();
        std::thread::spawn(move || {
            let res = psn::account_id_from_code(&code);
            later(move |s| {
                s.busy = false;
                match res {
                    Ok(id) => {
                        s.account = Some(id.number);
                        s.redirect = TextEdit::new("");
                        s.message = "Account-ID geholt. Jetzt den Code der PS5 eintragen.".into();
                    }
                    Err(e) => s.message = format!("{e:#}"),
                }
            });
        });
    }

    fn pair(s: &mut State) {
        if s.busy {
            return;
        }
        let Some(account) = s.account else {
            s.message = "Erst die Account-ID holen (Schritt 1).".into();
            return;
        };
        let Ok(pin) = s.pin.text.trim().parse::<u32>() else {
            s.message = "Der Code besteht aus den 8 Ziffern, die die PS5 anzeigt.".into();
            return;
        };
        let host = s.host.text.trim().to_string();
        if host.is_empty() {
            s.message = match pick_for_pairing(&s.found) {
                Ok(_) => "Bitte die PS5 oben auswählen.".into(),
                Err(_) if s.found.is_empty() => {
                    "Keine PS5 gefunden: „PS5 suchen“ drücken oder die IP-Adresse eintragen.".into()
                }
                Err(_) => "Bitte die PS5 oben auswählen (sie muss eingeschaltet sein).".into(),
            };
            return;
        }
        let path = s.path.clone();
        s.busy = true;
        s.message = "Koppeln …".into();
        std::thread::spawn(move || {
            let res = (|| -> anyhow::Result<Config> {
                let ip = resolve_host(&host)?;
                let reg = regist::register(ip, pin, account.to_le_bytes())?;
                cli::save_registration(&path, &host, &reg)?;
                Config::load(&path)
            })();
            later(move |s| {
                s.busy = false;
                match res {
                    Ok(cfg) => {
                        s.cfg = cfg;
                        s.pin = TextEdit::new("");
                        s.saved = s.cfg.to_text().unwrap_or_default();
                        s.pairing_open = false;
                        s.message = "Gekoppelt. Du kannst jetzt verbinden.".into();
                        check_power(s);
                    }
                    Err(e) => s.message = format!("Koppeln fehlgeschlagen: {e:#}"),
                }
            });
        });
    }

    fn search(s: &mut State) {
        if s.searching {
            return;
        }
        s.searching = true;
        std::thread::spawn(move || {
            let res = regist::discover(DISCOVER_WAIT);
            later(move |s| {
                s.searching = false;
                match res {
                    Ok(found) => {
                        // preselect the console when the choice is clear
                        if (s.host.text.trim().is_empty()
                            || !found.iter().any(|c| c.ip.to_string() == s.host.text.trim()))
                            && let Ok(c) = pick_for_pairing(&found)
                        {
                            s.host = TextEdit::new(c.ip.to_string());
                        }
                        if found.is_empty() {
                            s.message = "Keine PS5 im Netzwerk gefunden. Ist sie an und im selben Netz? Sonst die IP unten eintragen.".into();
                        }
                        s.found = found;
                    }
                    Err(e) => s.message = format!("Suche fehlgeschlagen: {e:#}"),
                }
            });
        });
    }

    /// Asks the paired PS5 whether it is on (in the background).
    fn check_power(s: &mut State) {
        if s.power_busy || s.cfg.credentials().is_err() {
            return;
        }
        s.power_busy = true;
        let cfg = s.cfg.clone();
        std::thread::spawn(move || {
            let res = power::status(&cfg);
            later(move |s| {
                s.power_busy = false;
                s.power = match res {
                    Ok((_, Power::On)) => PowerView::On,
                    Ok((_, Power::Rest)) => PowerView::Rest,
                    Err(e) => PowerView::Unreachable(format!("{e:#}")),
                };
            });
        });
    }

    fn wake(s: &mut State) {
        if s.power_busy {
            return;
        }
        s.power_busy = true;
        s.power = PowerView::Waking;
        let cfg = s.cfg.clone();
        std::thread::spawn(move || {
            let res = (|| -> anyhow::Result<PowerView> {
                power::wake(&cfg)?;
                // the PS5 takes a while; ask until it is on (wakeups are UDP: repeat them)
                for i in 0..20 {
                    std::thread::sleep(Duration::from_secs(2));
                    if let Ok((_, Power::On)) = power::status(&cfg) {
                        return Ok(PowerView::On);
                    }
                    if i % 5 == 4 {
                        let _ = power::wake(&cfg);
                    }
                }
                anyhow::bail!("die PS5 ist nicht aufgewacht (Einstellungen › System › Energiesparen › „Einschalten der PS5 über das Netzwerk aktivieren“ an?)")
            })();
            later(move |s| {
                s.power_busy = false;
                match res {
                    Ok(p) => s.power = p,
                    Err(e) => {
                        s.power = PowerView::Unknown;
                        s.message = format!("Einschalten fehlgeschlagen: {e:#}");
                    }
                }
            });
        });
    }

    fn rest(s: &mut State) {
        if s.power_busy {
            return;
        }
        // rest mode needs its own control connection: end ours first
        stop(s);
        s.power_busy = true;
        let cfg = s.cfg.clone();
        std::thread::spawn(move || {
            let res = power::sleep(&cfg);
            later(move |s| {
                s.power_busy = false;
                match res {
                    Ok(_) => s.power = PowerView::Rest,
                    Err(e) => s.message = format!("Ruhemodus fehlgeschlagen: {e:#}"),
                }
            });
        });
    }

    fn power_card(s: &State, t: Theme) -> El {
        let paired = s.cfg.credentials().is_ok();
        let line = if paired { s.power.label() } else { "Erst koppeln, dann lässt sich die PS5 hier ein- und ausschalten.".into() };
        let mut c = card("power", t)
            .child(text("PS5").font(type_::TITLE).color(t.text))
            .child(text(line).font(type_::LABEL).color(if s.power == PowerView::On { accent() } else { t.text_secondary }));
        if paired {
            let busy = s.power_busy;
            c = c.child(row().gap(10.0)
                .child(button(if busy && s.power == PowerView::Waking { "Schalte ein …" } else { "Einschalten" }, "wake", t, s.power == PowerView::Rest, wake))
                .child(button("Ruhemodus", "rest", t, false, rest))
                .child(button(if busy && s.power != PowerView::Waking { "Prüfe …" } else { "Zustand prüfen" }, "power-check", t, false, check_power)));
        }
        c
    }

    fn console_label(c: &FoundConsole) -> String {
        format!("{} · {} · {}", c.name, c.ip, if c.awake { "an" } else { "Ruhemodus" })
    }

    fn install_update(s: &mut State) {
        let Some(rel) = s.update.clone() else { return };
        if s.updating {
            return;
        }
        stop(s);
        s.updating = true;
        s.message = format!("Lade {} …", rel.tag);
        std::thread::spawn(move || {
            let res = update::install(&rel).and_then(|exe| update::restart(&exe));
            match res {
                // the new version is running now
                Ok(()) => std::process::exit(0),
                Err(e) => later(move |s| {
                    s.updating = false;
                    s.message = format!("Update fehlgeschlagen: {e:#}");
                }),
            }
        });
    }

    fn update_banner(s: &State, t: Theme) -> Option<El> {
        let rel = s.update.as_ref()?;
        let label = if s.updating { "Aktualisiere …" } else { "Aktualisieren" };
        Some(row().key("update").gap(12.0).padding(12.0).radius(12.0).center_y().fill(t.card).border(1.0, t.card_border)
            .child(text(format!("Version {} ist da", rel.tag)).font(type_::LABEL).color(t.text).grow(1.0))
            .child(button(label, "update-install", t, true, install_update)))
    }

    fn action_label(b: &Binding) -> String {
        match b {
            Binding::Simple(a) => a.clone(),
            Binding::Detailed(d) => d.action.clone(),
        }
    }

    fn remove_bind(s: &mut State, input: &str) {
        let i = sel_index(s);
        if let Some(p) = s.cfg.profiles.get_mut(i) {
            p.bind.remove(input);
        }
    }

    fn add_bind(s: &mut State) {
        let (input, action) = (s.bind_input.text.trim().to_string(), s.bind_action.text.trim().to_string());
        if input.is_empty() || action.is_empty() {
            s.message = "Taste und Aktion eintragen (Namen: `tast2controller keys`).".into();
            return;
        }
        let i = sel_index(s);
        let Some(p) = s.cfg.profiles.get_mut(i) else { return };
        let old = p.bind.insert(input.clone(), Binding::Simple(action));
        match Mapper::new(&s.cfg) {
            Ok(_) => {
                s.bind_input = TextEdit::new("");
                s.bind_action = TextEdit::new("");
                s.message = "Belegung hinzugefügt, gilt ab der nächsten Verbindung.".into();
            }
            Err(e) => {
                if let Some(p) = s.cfg.profiles.get_mut(i) {
                    match old {
                        Some(b) => p.bind.insert(input, b),
                        None => p.bind.remove(&input),
                    };
                }
                s.message = format!("Ungültig: {e:#}");
            }
        }
    }

    fn apply_template(s: &mut State, n: usize) {
        let Some(tp) = templates::TEMPLATES.get(n) else { return };
        let before = s.cfg.profiles.clone();
        let res = templates::apply(&mut s.cfg, tp).and_then(|replaced| Mapper::new(&s.cfg).map(|_| replaced));
        match res {
            Ok(replaced) => {
                let first = tp.profiles().ok().and_then(|p| p.first().map(|p| p.name.clone()));
                if let Some(i) = first.and_then(|f| s.cfg.profiles.iter().position(|p| p.name.eq_ignore_ascii_case(&f))) {
                    s.sel = i;
                }
                s.message = if replaced.is_empty() {
                    format!("Vorlage „{}“ hinzugefügt.", tp.label)
                } else {
                    format!("Vorlage „{}“ übernommen, ersetzt: {}.", tp.label, replaced.join(", "))
                };
            }
            Err(e) => {
                s.cfg.profiles = before;
                s.message = format!("Vorlage passt nicht zur Config: {e:#}");
            }
        }
    }

    fn remove_profile(s: &mut State) {
        let i = sel_index(s);
        if s.cfg.profiles.len() <= 1 {
            s.message = "Das letzte Profil bleibt stehen.".into();
            return;
        }
        let removed = s.cfg.profiles.remove(i);
        if let Err(e) = Mapper::new(&s.cfg) {
            s.cfg.profiles.insert(i, removed);
            s.message = format!("Profil wird noch gebraucht: {e:#}");
            return;
        }
        if s.cfg.input.start_profile.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&removed.name)) {
            s.cfg.input.start_profile = None;
        }
        s.sel = i.saturating_sub(1);
        s.message = format!("Profil „{}“ gelöscht.", removed.name);
    }

    fn connect_tab(s: &State, t: Theme) -> El {
        let running = s.runner.is_some();
        let connection = card("connection", t)
            .child(text("Verbindung").font(type_::TITLE).color(t.text))
            .child(text(if running { s.status.clone() } else { "Nicht verbunden".into() }).font(type_::LABEL).color(t.text_secondary));
        let connection = if running {
            let (line, color) = if s.capturing {
                (format!("Maus und Tastatur steuern die PS5. {} gibt sie frei, {} trennt.",
                    s.cfg.input.capture_toggle, s.cfg.input.quit), accent())
            } else {
                (format!("Maus und Tastatur sind frei. „Steuerung übernehmen“ oder {} schickt sie an die PS5.",
                    s.cfg.input.capture_toggle), t.text_secondary)
            };
            connection.child(text(line).font(type_::LABEL).color(color))
        } else {
            connection
        };
        let mut buttons = row().gap(10.0);
        buttons = if running {
            buttons.child(button("Steuerung übernehmen", "capture", t, !s.capturing, take_input))
                .child(button("Trennen", "stop", t, false, stop))
        } else {
            buttons.child(button("Verbinden", "start", t, true, start))
        };
        let paired = s.cfg.credentials().is_ok();
        let connection = if paired {
            connection.child(buttons)
        } else {
            connection.child(text("Zuerst die PS5 koppeln (unten).").font(type_::LABEL).color(t.text_secondary))
        };
        let mut consoles = row().gap(8.0).flex_wrap().center_y();
        for c in &s.found {
            let ip = c.ip.to_string();
            let selected = s.host.text.trim() == ip;
            consoles = consoles.child(chip(&console_label(c), format!("console-{ip}"), selected, t,
                move |s: &mut State| s.host = TextEdit::new(ip.clone())));
        }
        consoles = consoles.child(button(if s.searching { "Suche läuft …" } else { "PS5 suchen" }, "search", t, s.found.is_empty(), search));
        if paired && !s.pairing_open {
            let label = if s.cfg.console.nickname.is_empty() { "PS5".to_string() } else { s.cfg.console.nickname.clone() };
            let pairing = card("pairing", t)
                .child(row().gap(10.0).center_y()
                    .child(text(format!("Gekoppelt mit {label}")).font(type_::LABEL).color(t.text).grow(1.0))
                    .child(button("Neu koppeln", "pair-open", t, false, |s: &mut State| s.pairing_open = true)));
            return col().gap(16.0).child(connection).child(power_card(s, t)).child(pairing);
        }
        let pairing = card("pairing", t)
            .child(text("PS5 koppeln").font(type_::TITLE).color(t.text))
            .child(text("1. Bei Sony anmelden").font(type_::LABEL).color(t.text_secondary))
            .child(row().gap(10.0).center_y()
                .child(button("Sony-Anmeldung öffnen", "open-login", t, false, open_login))
                .child(field("redirect", &s.redirect, "Adresse der Seite „redirect“ hier einfügen", t, |s| &mut s.redirect))
                .child(button("Account-ID holen", "get-account", t, false, fetch_account)))
            .child(text(if s.account.is_some() { "Account-ID: geholt" } else { "Account-ID: noch nicht geholt" }).font(type_::LABEL).color(t.text))
            .child(text("2. PS5 auswählen").font(type_::LABEL).color(t.text_secondary))
            .child(consoles)
            .child(row().gap(10.0).center_y()
                .child(text("oder IP manuell:").font(type_::LABEL).color(t.text_secondary))
                .child(field("host", &s.host, "z. B. 192.168.1.50", t, |s| &mut s.host)))
            .child(text("3. Code aus PS5 › Remote-Wiedergabe › Gerät verknüpfen").font(type_::LABEL).color(t.text_secondary))
            .child(row().gap(10.0).center_y()
                .child(field("pin", &s.pin, "8-stelliger Code", t, |s| &mut s.pin))
                .child(button("Koppeln", "pair", t, true, pair)));
        let pairing = if paired {
            pairing.child(row().child(button("Schließen", "pair-close", t, false, |s: &mut State| s.pairing_open = false)))
        } else {
            pairing
        };
        col().gap(16.0).child(connection).child(power_card(s, t)).child(pairing)
    }

    fn check_update(s: &mut State) {
        if s.checking {
            return;
        }
        s.checking = true;
        std::thread::spawn(|| {
            let res = update::check();
            later(move |s| {
                s.checking = false;
                match res {
                    Ok(Some(rel)) => s.update = Some(rel),
                    Ok(None) => s.message = "Du hast die neueste Version.".into(),
                    Err(e) => s.message = format!("Update-Prüfung fehlgeschlagen: {e:#}"),
                }
            });
        });
    }

    fn settings_tab(s: &State, t: Theme) -> El {
        let ui = &s.cfg.ui;
        let line = |label: &str, value: String| {
            row().gap(10.0).center_y().h(30.0)
                .child(text(label.to_string()).font(type_::LABEL).color(t.text_secondary).w(180.0))
                .child(text(value).font(type_::LABEL).color(t.text).grow(1.0))
        };
        let start = card("settings-start", t)
            .child(text("Start").font(type_::TITLE).color(t.text))
            .child(row().gap(8.0).flex_wrap()
                .child(chip("Im Tray starten", "ui-tray".into(), ui.start_in_tray, t, |s: &mut State| s.cfg.ui.start_in_tray = !s.cfg.ui.start_in_tray))
                .child(chip("Beim Start verbinden", "ui-autoconnect".into(), ui.connect_on_launch, t, |s: &mut State| s.cfg.ui.connect_on_launch = !s.cfg.ui.connect_on_launch))
                .child(chip("Steuerung sofort übernehmen", "ui-capture".into(), s.cfg.input.capture_on_start, t, |s: &mut State| s.cfg.input.capture_on_start = !s.cfg.input.capture_on_start)));
        let keys = card("settings-keys", t)
            .child(text("Tasten").font(type_::TITLE).color(t.text))
            .child(line("Freigeben / übernehmen", s.cfg.input.capture_toggle.clone()))
            .child(line("Trennen", s.cfg.input.quit.clone()));
        let about = card("settings-about", t)
            .child(text("Programm").font(type_::TITLE).color(t.text))
            .child(line("Version", update::current_version().to_string()))
            .child(line("Config", s.path.display().to_string()))
            .child(row().child(button(if s.checking { "Prüfe …" } else { "Nach Updates suchen" }, "update-check", t, false, check_update)));
        col().gap(16.0).child(start).child(keys).child(about)
    }

    fn keys_tab(s: &State, t: Theme) -> El {
        let i = sel_index(s);
        let mut chips = row().gap(8.0).flex_wrap();
        for (n, p) in s.cfg.profiles.iter().enumerate() {
            chips = chips.child(chip(&p.name, format!("profile-{n}"), n == i, t, move |s: &mut State| s.sel = n));
        }
        let mut list = col().key("binds").gap(6.0);
        if let Some(p) = s.cfg.profiles.get(i) {
            let is_start = s.cfg.input.start_profile.as_deref().map(|n| n.eq_ignore_ascii_case(&p.name)).unwrap_or(i == 0);
            list = list.child(text(if is_start { "Startprofil" } else { "Kein Startprofil" }).font(type_::LABEL).color(t.text_secondary));
            for (input, binding) in &p.bind {
                let name = input.clone();
                list = list.child(row().gap(10.0).center_y().h(30.0)
                    .child(text(input.clone()).font(type_::LABEL).color(t.text).w(120.0))
                    .child(text(format!("→ {}", action_label(binding))).font(type_::LABEL).color(t.text_secondary).grow(1.0))
                    .child(button("✕", &format!("rm-{input}"), t, false, move |s: &mut State| remove_bind(s, &name))));
            }
        }
        let add = card("add-bind", t)
            .child(text("Belegung hinzufügen").font(type_::TITLE).color(t.text))
            .child(row().gap(10.0).center_y()
                .child(field("bind-input", &s.bind_input, "Taste, z. B. W oder MouseLeft", t, |s| &mut s.bind_input))
                .child(field("bind-action", &s.bind_action, "Aktion, z. B. cross oder left_stick_up", t, |s| &mut s.bind_action))
                .child(button("Hinzufügen", "bind-add", t, true, add_bind)));
        let start_btn = button("Als Startprofil setzen", "set-start", t, false, |s: &mut State| {
            let i = sel_index(s);
            if let Some(p) = s.cfg.profiles.get(i) {
                s.cfg.input.start_profile = Some(p.name.clone());
            }
        });
        let mut presets = card("templates", t)
            .child(text("Vorlagen").font(type_::TITLE).color(t.text))
            .child(text("Gleichnamige Profile werden ersetzt.").font(type_::LABEL).color(t.text_secondary));
        for (n, tp) in templates::TEMPLATES.iter().enumerate() {
            presets = presets.child(row().gap(10.0).center_y()
                .child(button(tp.label, &format!("template-{n}"), t, false, move |s: &mut State| apply_template(s, n)))
                .child(text(tp.about).font(type_::LABEL).color(t.text_secondary).grow(1.0)));
        }
        let remove_btn = button("Profil löschen", "profile-remove", t, false, remove_profile);
        col().gap(14.0).child(chips).child(row().gap(10.0).child(start_btn).child(remove_btn))
            .child(card("bind-list", t).child(list)).child(add).child(presets)
    }

    fn mouse_tab(s: &State, t: Theme) -> El {
        let i = sel_index(s);
        let Some(p) = s.cfg.profiles.get(i) else {
            return col().child(text("Kein Profil in der Config.").font(type_::LABEL).color(t.text_secondary));
        };
        let m = p.mouse.engine.clone();
        let target = p.mouse.target;
        let set_target = |tg: MouseTarget| move |s: &mut State| {
            let i = sel_index(s);
            if let Some(p) = s.cfg.profiles.get_mut(i) {
                p.mouse.target = tg;
            }
        };
        let targets = row().gap(8.0).flex_wrap()
            .child(chip("Rechter Stick", "tg-right".into(), target == MouseTarget::RightStick, t, set_target(MouseTarget::RightStick)))
            .child(chip("Linker Stick", "tg-left".into(), target == MouseTarget::LeftStick, t, set_target(MouseTarget::LeftStick)))
            .child(chip("Gyro", "tg-gyro".into(), target == MouseTarget::Gyro, t, set_target(MouseTarget::Gyro)))
            .child(chip("Aus", "tg-none".into(), target == MouseTarget::None, t, set_target(MouseTarget::None)))
            .child(chip(if m.invert_y { "Y invertiert" } else { "Y normal" }, "invert-y".into(), m.invert_y, t,
                |s: &mut State| nudge(s, |m| m.invert_y = !m.invert_y)));
        let stepper = |label: &str, key: &str, value: f32, down: fn(&mut State), up: fn(&mut State)| {
            row().gap(10.0).center_y()
                .child(text(label.to_string()).font(type_::LABEL).color(t.text_secondary).w(140.0))
                .child(button("−", &format!("{key}-down"), t, false, down))
                .child(text(format!("{value:.2}")).font(type_::LABEL).color(t.text).w(60.0))
                .child(button("+", &format!("{key}-up"), t, false, up))
        };
        let tuning = card("tuning", t)
            .child(text(format!("Maus: Profil {}", p.name)).font(type_::TITLE).color(t.text))
            .child(targets)
            .child(stepper("Empfindlichkeit", "sens", m.sensitivity, |s| nudge(s, |m| m.sensitivity -= 0.01), |s| nudge(s, |m| m.sensitivity += 0.01)))
            .child(stepper("Kurve", "curve", m.curve, |s| nudge(s, |m| m.curve -= 0.1), |s| nudge(s, |m| m.curve += 0.1)))
            .child(stepper("Anti-Deadzone", "adz", m.anti_deadzone, |s| nudge(s, |m| m.anti_deadzone -= 0.01), |s| nudge(s, |m| m.anti_deadzone += 0.01)))
            .child(stepper("Beschleunigung", "accel", m.acceleration, |s| nudge(s, |m| m.acceleration -= 0.1), |s| nudge(s, |m| m.acceleration += 0.1)))
            .child(stepper("Glättung (ms)", "smooth", m.smoothing_ms, |s| nudge(s, |m| m.smoothing_ms -= 1.0), |s| nudge(s, |m| m.smoothing_ms += 1.0)))
            .child(stepper("ADS-Faktor (L2)", "ads", m.ads_multiplier, |s| nudge(s, |m| m.ads_multiplier -= 0.05), |s| nudge(s, |m| m.ads_multiplier += 0.05)));
        let mut chips = row().gap(8.0).flex_wrap();
        for (n, p) in s.cfg.profiles.iter().enumerate() {
            chips = chips.child(chip(&p.name, format!("mprofile-{n}"), n == i, t, move |s: &mut State| s.sel = n));
        }
        col().gap(14.0).child(chips).child(tuning)
    }

    fn view(s: &State, cx: &mut Cx<State>) -> El {
        let t = tok::theme(cx.env().system_dark);
        let tabs = row().gap(8.0)
            .child(chip("Verbindung", "tab-connect".into(), s.tab == Tab::Connect, t, |s: &mut State| s.tab = Tab::Connect))
            .child(chip("Profile & Tasten", "tab-keys".into(), s.tab == Tab::Keys, t, |s: &mut State| s.tab = Tab::Keys))
            .child(chip("Maus", "tab-mouse".into(), s.tab == Tab::Mouse, t, |s: &mut State| s.tab = Tab::Mouse))
            .child(chip("Einstellungen", "tab-settings".into(), s.tab == Tab::Settings, t, |s: &mut State| s.tab = Tab::Settings));
        let body = match s.tab {
            Tab::Connect => connect_tab(s, t),
            Tab::Keys => keys_tab(s, t),
            Tab::Mouse => mouse_tab(s, t),
            Tab::Settings => settings_tab(s, t),
        };
        let (pill, pill_on) = match (&s.runner, s.capturing) {
            (Some(_), true) => ("Steuert die PS5", true),
            (Some(r), false) if r.is_streaming() => ("Verbunden", true),
            (Some(_), false) => ("Verbindet …", false),
            (None, _) => ("Nicht verbunden", false),
        };
        let header = row().gap(12.0).center_y()
            .child(text("Tast2ControllerRust").font(type_::TITLE).color(t.text))
            .child(text(format!("v{}", update::current_version())).font(type_::LABEL).color(t.text_secondary).grow(1.0))
            .child(row().key("status-pill").h(28.0).px(12.0).radius(14.0).center_y()
                .fill(if pill_on { accent() } else { t.card }).border(1.0, t.card_border)
                .child(text(pill).font(type_::LABEL).color(if pill_on { Color::WHITE } else { t.text_secondary })));
        let mut page = col().w_full().h_full().padding(28.0).gap(14.0).fill(t.bg)
            .child(header)
            .child(tabs);
        if let Some(banner) = update_banner(s, t) {
            page = page.child(banner);
        }
        page = page
            .child(col().key(format!("body-{}", s.tab as u8)).child(body).scroll(rune::ScrollSpec { stick_bottom: false }).shrink().min_h(0.0).grow(1.0));
        if !s.message.is_empty() {
            page = page.child(row().key("message").gap(10.0).padding(10.0).radius(10.0).center_y().fill(t.card).border(1.0, t.card_border)
                .child(text(s.message.clone()).font(type_::LABEL).color(t.text).grow(1.0))
                .child(button("✕", "message-close", t, false, |s: &mut State| s.message.clear())));
        }
        page
    }

    pub fn run() {
        // title bar, taskbar and Alt+Tab icon (must be set before the window opens)
        if let Err(e) = rune::set_app_icon(include_bytes!("../../assets/icon/tast2controller-256.png").as_slice()) {
            log::warn!("window icon: {e}");
        }
        let mut spec = AppSpec::new("Tast2ControllerRust", (760.0, 720.0), State::default, view);
        spec.min_size = Some((600.0, 520.0));
        let app = App::new(spec);
        let _ = HANDLE.set(app.handle());
        // Look for a newer release once; stay quiet when offline.
        std::thread::spawn(|| match update::check() {
            Ok(Some(rel)) => later(move |s| s.update = Some(rel)),
            Ok(None) => {}
            Err(e) => log::info!("update check: {e:#}"),
        });
        // Refresh the connection status twice a second.
        std::thread::spawn(|| {
            // Look for consoles right away so pairing needs no IP address.
            std::thread::sleep(Duration::from_millis(300));
            later(|s| init_window(s, 10));
            later(search);
            later(check_power);
            loop {
                std::thread::sleep(Duration::from_millis(500));
                if let Some(h) = HANDLE.get() {
                    h.update(|s, _| {
                        if let Some(r) = &s.runner {
                            s.status = r.run_state().phase.label();
                            s.capturing = r.is_capturing();
                            if r.is_finished() {
                                s.runner = None;
                                s.capturing = false;
                            }
                        }
                        autosave(s);
                        fade_message(s);
                        if let Some(tray) = &s.tray {
                            tray.set_status(&format!("Tast2ControllerRust: {}", s.status), s.runner.is_some());
                        }
                    });
                }
            }
        });
        app.run();
    }
}
