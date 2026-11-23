//! Settings window for Tast2ControllerRust: connection status, start/stop, profile and mouse tuning.
//! Built with Rune like the Schmidhome desktop app. Windows only (input capture is Windows only).

#[cfg(not(windows))]
fn main() {
    eprintln!("the settings window is only available on Windows");
}

#[cfg(windows)]
fn main() {
    ui::run();
}

#[cfg(windows)]
mod ui {
    use std::path::PathBuf;
    use std::sync::OnceLock;
    use std::time::Duration;

    use rune::tok::{self, Theme, type_};
    use rune::{App, AppHandle, AppSpec, Color, Cx, El, col, row, text};
    use tast2controller::config::Config;
    use tast2controller::runner::{CaptureIn, Runner};

    static HANDLE: OnceLock<AppHandle<State>> = OnceLock::new();

    struct State {
        path: PathBuf,
        cfg: Config,
        runner: Option<Runner>,
        status: String,
        message: String,
    }

    fn config_path() -> PathBuf {
        let name = "tast2controller.toml";
        let cwd = PathBuf::from(name);
        if cwd.exists() {
            return cwd;
        }
        match std::env::var_os("APPDATA") {
            Some(dir) => PathBuf::from(dir).join("tast2controller").join(name),
            None => cwd,
        }
    }

    impl Default for State {
        fn default() -> Self {
            let path = config_path();
            let (cfg, message) = match Config::load(&path) {
                Ok(c) => (c, String::new()),
                Err(e) => (Config::default_config(), format!("Config nicht geladen: {e:#}")),
            };
            State { path, cfg, runner: None, status: "Nicht verbunden".into(), message }
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

    /// One tunable number with minus and plus buttons.
    fn stepper(label: &str, key: &str, value: f32, t: Theme, down: fn(&mut State), up: fn(&mut State)) -> El {
        row().gap(10.0).center_y()
            .child(text(label.to_string()).font(type_::LABEL).color(t.text_secondary).w(130.0))
            .child(button("−", &format!("{key}-down"), t, false, down))
            .child(text(format!("{value:.2}")).font(type_::LABEL).color(t.text).w(60.0))
            .child(button("+", &format!("{key}-up"), t, false, up))
    }

    fn nudge(s: &mut State, f: impl Fn(&mut tast2controller_input::MouseConfig)) {
        if let Some(p) = s.cfg.profiles.first_mut() {
            f(&mut p.mouse.engine);
            p.mouse.engine = p.mouse.engine.clone().sanitized();
        }
    }

    fn save(s: &mut State) {
        match s.cfg.save(&s.path) {
            Ok(()) => s.message = format!("Gespeichert: {}", s.path.display()),
            Err(e) => s.message = format!("Speichern fehlgeschlagen: {e:#}"),
        }
    }

    fn start(s: &mut State) {
        if s.runner.is_some() {
            return;
        }
        match Runner::start(&s.cfg, CaptureIn::OwnWindow) {
            Ok(r) => {
                s.runner = Some(r);
                s.message.clear();
            }
            Err(e) => s.message = format!("Start fehlgeschlagen: {e:#}"),
        }
    }

    fn stop(s: &mut State) {
        if let Some(r) = s.runner.take() {
            r.request_stop();
            let _ = r.stop_and_wait();
        }
        s.status = "Nicht verbunden".into();
    }

    fn view(s: &State, cx: &mut Cx<State>) -> El {
        let t = tok::theme(cx.env().system_dark);
        let running = s.runner.is_some();
        let profile = s.runner.as_ref().map(|r| r.active_profile()).unwrap_or_else(|| {
            s.cfg.profiles.first().map(|p| p.name.clone()).unwrap_or_default()
        });
        let m = s.cfg.profiles.first().map(|p| p.mouse.engine.clone()).unwrap_or_default();

        let header = col().gap(4.0)
            .child(text("Tast2ControllerRust").font(type_::TITLE).color(t.text))
            .child(text(s.status.clone()).font(type_::LABEL).color(t.text_secondary));

        let controls = row().gap(10.0)
            .child(if running {
                button("Trennen", "stop", t, false, stop)
            } else {
                button("Verbinden", "start", t, true, start)
            })
            .child(button("Config speichern", "save", t, false, save));

        let tuning = col().key("tuning").gap(10.0).padding(16.0).radius(12.0).fill(t.card).border(1.0, t.card_border)
            .child(text(format!("Maus (Profil: {profile})")).font(type_::TITLE).color(t.text))
            .child(stepper("Empfindlichkeit", "sens", m.sensitivity, t,
                |s| nudge(s, |m| m.sensitivity -= 0.01), |s| nudge(s, |m| m.sensitivity += 0.01)))
            .child(stepper("Kurve", "curve", m.curve, t,
                |s| nudge(s, |m| m.curve -= 0.1), |s| nudge(s, |m| m.curve += 0.1)))
            .child(stepper("Anti-Deadzone", "adz", m.anti_deadzone, t,
                |s| nudge(s, |m| m.anti_deadzone -= 0.01), |s| nudge(s, |m| m.anti_deadzone += 0.01)))
            .child(stepper("Beschleunigung", "accel", m.acceleration, t,
                |s| nudge(s, |m| m.acceleration -= 0.1), |s| nudge(s, |m| m.acceleration += 0.1)))
            .child(stepper("Glättung (ms)", "smooth", m.smoothing_ms, t,
                |s| nudge(s, |m| m.smoothing_ms -= 1.0), |s| nudge(s, |m| m.smoothing_ms += 1.0)))
            .child(stepper("ADS-Faktor (L2)", "ads", m.ads_multiplier, t,
                |s| nudge(s, |m| m.ads_multiplier -= 0.05), |s| nudge(s, |m| m.ads_multiplier += 0.05)))
            .child(text("Änderungen wirken nach „Config speichern“ und neuem Verbinden.").font(type_::LABEL).color(t.text_secondary));

        let mut page = col().w_full().h_full().padding(28.0).gap(16.0).fill(t.bg)
            .child(header).child(controls).child(tuning);
        if !s.message.is_empty() {
            page = page.child(text(s.message.clone()).font(type_::LABEL).color(t.text_secondary));
        }
        page
    }

    pub fn run() {
        let mut spec = AppSpec::new("Tast2ControllerRust", (640.0, 640.0), State::default, view);
        spec.min_size = Some((520.0, 520.0));
        let app = App::new(spec);
        let _ = HANDLE.set(app.handle());
        // Refresh the connection status twice a second.
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_millis(500));
            if let Some(h) = HANDLE.get() {
                h.update(|s, _| {
                    if let Some(r) = &s.runner {
                        s.status = r.run_state().phase.label();
                        if r.is_finished() {
                            s.runner = None;
                        }
                    }
                });
            }
        });
        app.run();
    }
}
