//! A running session: the session thread (wake, connect, stream, reconnect) plus input
//! capture, either in its own small window or in the settings window. Can be started and
//! stopped any number of times.

use std::net::{IpAddr, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::config::{Config, Credentials};
use crate::input;
use crate::input::windows::{HostOptions, WindowHandle};
use crate::{regist, session, stream};

pub use crate::resolve_host;

/// Sends a wakeup and waits until the console answers discovery as running.
fn wake_and_wait(
    ip: IpAddr,
    creds: &Credentials,
    stop: &AtomicBool,
    status: &dyn Fn(&str),
) -> Result<()> {
    status("Console not reachable, waking it up ...");
    regist::wakeup(ip, creds)?;
    let deadline = Instant::now() + Duration::from_secs(45);
    let mut last_wakeup = Instant::now();
    while Instant::now() < deadline && !stop.load(Ordering::Relaxed) {
        if regist::probe(ip)
            .map(|s| s.starts_with("200"))
            .unwrap_or(false)
        {
            thread::sleep(Duration::from_secs(2)); // let Remote Play come up
            return Ok(());
        }
        if last_wakeup.elapsed() > Duration::from_secs(10) {
            let _ = regist::wakeup(ip, creds); // UDP: repeat in case it was lost
            last_wakeup = Instant::now();
        }
        thread::sleep(Duration::from_millis(500));
    }
    if stop.load(Ordering::Relaxed) {
        return Ok(());
    }
    bail!("the console did not wake up (is \"Enable Turning On PS5 from Network\" on?)")
}

/// True for errors that mean the console isn't listening (off / rest mode), as opposed to
/// the console answering and refusing.
fn is_unreachable(e: &anyhow::Error) -> bool {
    e.chain()
        .any(|c| c.downcast_ref::<std::io::Error>().is_some())
}

/// Where a session is, in words a user understands.
#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Starting,
    Waking,
    Connecting,
    SigningIn,
    StartingStream,
    Ready,
    Retrying { at: Instant },
    Stopped,
}

impl Phase {
    pub fn label(&self) -> String {
        match self {
            Phase::Starting => "Starting…".into(),
            Phase::Waking => "Waking up the PS5…".into(),
            Phase::Connecting => "Connecting…".into(),
            Phase::SigningIn => "Signing in…".into(),
            Phase::StartingStream => "Starting the controller connection…".into(),
            Phase::Ready => "Connected".into(),
            Phase::Retrying { at } => {
                format!(
                    "Trying again in {}s…",
                    at.saturating_duration_since(Instant::now()).as_secs() + 1
                )
            }
            Phase::Stopped => "Not connected".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemKind {
    Unreachable,
    NotPaired,
    InUse,
    PasscodeNeeded,
    PasscodeWrong,
    WakeFailed,
    Dropped,
    Other,
}

/// An error explained: what happened, what to do, and the technical detail.
#[derive(Clone, Debug)]
pub struct Problem {
    pub kind: ProblemKind,
    pub title: String,
    pub fix: String,
    pub detail: String,
}

impl Problem {
    /// Whether trying again on its own can help.
    pub fn retryable(&self) -> bool {
        matches!(
            self.kind,
            ProblemKind::Unreachable
                | ProblemKind::InUse
                | ProblemKind::Dropped
                | ProblemKind::Other
        )
    }
}

pub fn explain(e: &anyhow::Error, host: &str) -> Problem {
    let detail = format!("{e:#}");
    let p = |kind, title: &str, fix: String| Problem {
        kind,
        title: title.into(),
        fix,
        detail: detail.clone(),
    };
    if let Some(l) = e
        .chain()
        .find_map(|c| c.downcast_ref::<session::LoginError>())
    {
        return match l {
            session::LoginError::PasscodeNeeded => p(
                ProblemKind::PasscodeNeeded,
                "Your PS5 asks for your login passcode",
                "Enter the 4-digit passcode you use to sign in on the PS5.".into(),
            ),
            session::LoginError::PasscodeWrong => p(
                ProblemKind::PasscodeWrong,
                "The PS5 rejected the login passcode",
                "Check the 4-digit passcode you use to sign in on the PS5 and enter it again."
                    .into(),
            ),
        };
    }
    if let Some(r) = e
        .chain()
        .find_map(|c| c.downcast_ref::<session::SessionRejected>())
    {
        if r.in_use() {
            return p(
                ProblemKind::InUse,
                "Remote Play is already in use",
                "Another device (phone, PC or PS Portal) is connected to the PS5. Disconnect it; this app keeps trying.".into(),
            );
        }
        return p(
            ProblemKind::NotPaired,
            "The PS5 doesn't recognise this PC",
            "The pairing may have been removed on the PS5. Run the setup again to link this PC."
                .into(),
        );
    }
    if detail.contains("did not wake up") {
        return p(
            ProblemKind::WakeFailed,
            "The PS5 didn't wake up",
            "Turn it on with the controller, or on the PS5 enable Settings › System › Power Saving › \
             Features Available in Rest Mode › Stay Connected to the Internet and Enable Turning On PS5 from Network."
                .into(),
        );
    }
    if detail.contains("Takion") || detail.contains("console closed the stream port") {
        return p(
            ProblemKind::Dropped,
            "The connection to the PS5 dropped",
            "This app reconnects by itself.".into(),
        );
    }
    if is_unreachable(e) {
        return p(
            ProblemKind::Unreachable,
            "Can't reach the PS5",
            format!(
                "Check that the PS5 is on (or in rest mode with network wake enabled), that Remote Play is enabled \
                 (Settings › System › Remote Play), and that it is on the same network as this PC at {host}."
            ),
        );
    }
    if detail.contains("BANG")
        || detail.contains("stream info")
        || detail.contains("Takion")
        || detail.contains("stream connection")
    {
        return p(
            ProblemKind::Dropped,
            "The controller connection didn't start",
            "The PS5 stopped answering while setting up. This app tries again; if it keeps failing, restart the PS5.".into(),
        );
    }
    p(
        ProblemKind::Other,
        "Something went wrong",
        "This app tries again. The details below say what failed.".into(),
    )
}

#[derive(Clone, Debug)]
pub struct RunState {
    pub phase: Phase,
    /// The latest problem; cleared once connected.
    pub problem: Option<Problem>,
}

/// Where keyboard and mouse are captured.
#[derive(Clone, Copy)]
pub enum CaptureIn {
    /// a small window of its own (command line)
    OwnWindow,
    /// the settings window (its handle), prepared with `input::windows::install_in_window`
    Window(isize),
}

pub struct Runner {
    stop: Arc<AtomicBool>,
    window: Arc<Mutex<Option<WindowHandle>>>,
    link: Arc<stream::ControllerLink>,
    state: Arc<Mutex<RunState>>,
    active_profile: Arc<Mutex<String>>,
    cfg: Arc<Config>,
    session: Option<JoinHandle<()>>,
    /// applies input to the mapper; ends when capture ends
    pump: Option<JoinHandle<()>>,
    /// the own capture window's thread
    own_window: Option<JoinHandle<Result<()>>>,
}

impl Runner {
    /// Validates the config and starts the session and input capture.
    pub fn start(cfg: &Config, capture_in: CaptureIn) -> Result<Runner> {
        let creds = cfg.credentials()?;
        if cfg.console.host.trim().is_empty() {
            bail!("no console address set");
        }
        let ip = resolve_host(&cfg.console.host)?;
        let mut profile = stream::VideoProfile::preset(&cfg.stream.resolution, cfg.stream.fps)?;
        if let Some(b) = cfg.stream.bitrate_kbps {
            profile.bitrate_kbps = b;
        }
        let mapper = input::mapper::Mapper::new(cfg)?;
        let active_profile = Arc::new(Mutex::new(mapper.profile_name().to_string()));
        let capture_key = input::keys::parse(&cfg.input.capture_toggle)
            .context("capture toggle key is not a known key")?;
        let quit_key =
            input::keys::parse(&cfg.input.quit).context("quit key is not a known key")?;
        let tick = Duration::from_secs_f64(1.0 / cfg.input.tick_hz.clamp(50, 1000) as f64);

        let link = stream::ControllerLink::new();
        let status = Arc::new(Mutex::new(String::from("Starting ...")));
        let window: Arc<Mutex<Option<WindowHandle>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(RunState {
            phase: Phase::Starting,
            problem: None,
        }));

        let (tx, rx) = std::sync::mpsc::channel();
        let opts = HostOptions {
            capture_key,
            quit_key,
            capture_on_start: cfg.input.capture_on_start,
            stop: stop.clone(),
            status: status.clone(),
            active_profile: active_profile.clone(),
        };
        let own_window = match capture_in {
            CaptureIn::Window(hwnd) => {
                *window.lock().unwrap() = Some(input::windows::attach_to_window(hwnd, tx, opts)?);
                None
            }
            CaptureIn::OwnWindow => {
                let window = window.clone();
                let status = status.clone();
                let stop = stop.clone();
                Some(
                    thread::Builder::new()
                        .name("capture".into())
                        .spawn(move || {
                            let r = input::windows::run_window(tx, opts, |h| {
                                *window.lock().unwrap() = Some(h)
                            });
                            *window.lock().unwrap() = None;
                            if let Err(e) = &r {
                                log::error!("capture window: {e:#}");
                                *status.lock().unwrap() = format!("Capture window failed: {e:#}");
                            }
                            // closing the capture window ends the session
                            stop.store(true, Ordering::Relaxed);
                            r
                        })?,
                )
            }
        };

        let pump = {
            let link = link.clone();
            let active_profile = active_profile.clone();
            let window = window.clone();
            thread::Builder::new().name("input".into()).spawn(move || {
                input::windows::pump(mapper, link, rx, active_profile, tick, || {
                    if let Some(w) = *window.lock().unwrap() {
                        w.refresh();
                    }
                })
            })?
        };

        let session = {
            let ctx = SessionCtx {
                link: link.clone(),
                status: status.clone(),
                window: window.clone(),
                stop: stop.clone(),
                state: state.clone(),
            };
            let cfg = cfg.clone();
            thread::Builder::new()
                .name("session".into())
                .spawn(move || session_loop(cfg, creds, ip, profile, ctx))?
        };

        Ok(Runner {
            stop,
            window,
            link,
            state,
            active_profile,
            cfg: Arc::new(cfg.clone()),
            session: Some(session),
            pump: Some(pump),
            own_window,
        })
    }

    pub fn is_streaming(&self) -> bool {
        self.link.is_streaming()
    }

    pub fn run_state(&self) -> RunState {
        self.state.lock().unwrap().clone()
    }

    /// Whether keyboard and mouse go to the PS5 right now.
    pub fn is_capturing(&self) -> bool {
        input::windows::is_capturing()
    }

    /// Takes over keyboard and mouse (if the window is in front) or gives them back.
    pub fn set_capture(&self, on: bool) {
        if let Some(w) = *self.window.lock().unwrap() {
            w.set_capture(on);
        }
    }

    /// Name of the profile in use right now.
    pub fn active_profile(&self) -> String {
        self.active_profile
            .lock()
            .map(|p| p.clone())
            .unwrap_or_default()
    }

    /// The config this session runs with (bindings may differ from unsaved edits).
    pub fn config(&self) -> Arc<Config> {
        self.cfg.clone()
    }

    pub fn controller_state(&self) -> crate::controller::ControllerState {
        self.link.state()
    }

    /// True once capture and the session thread have ended.
    pub fn is_finished(&self) -> bool {
        fn done<T>(t: &Option<JoinHandle<T>>) -> bool {
            t.as_ref().map(|t| t.is_finished()).unwrap_or(true)
        }
        done(&self.session) && done(&self.pump) && done(&self.own_window)
    }

    /// Whether the session was told to stop (as opposed to giving up by itself).
    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// Asks everything to stop without waiting.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = *self.window.lock().unwrap() {
            w.close();
        }
    }

    /// Stops and waits (up to the socket timeouts, a few seconds).
    pub fn stop_and_wait(mut self) -> Result<()> {
        self.request_stop();
        let ui = self.own_window.take().map(|t| t.join());
        for t in [self.pump.take(), self.session.take()]
            .into_iter()
            .flatten()
        {
            let _ = t.join();
        }
        match ui {
            Some(Ok(r)) => r,
            Some(Err(_)) => bail!("capture window thread panicked"),
            None => Ok(()),
        }
    }

    /// Runs until the capture window is closed (used by the command line `run`).
    pub fn wait(mut self) -> Result<()> {
        let r = self.own_window.take().map(|t| t.join());
        self.stop_and_wait()?;
        match r {
            Some(Ok(r)) => r,
            Some(Err(_)) => bail!("capture window thread panicked"),
            None => Ok(()),
        }
    }
}

impl Drop for Runner {
    fn drop(&mut self) {
        self.request_stop();
    }
}

struct SessionCtx {
    link: Arc<stream::ControllerLink>,
    status: Arc<Mutex<String>>,
    window: Arc<Mutex<Option<WindowHandle>>>,
    stop: Arc<AtomicBool>,
    state: Arc<Mutex<RunState>>,
}

fn session_loop(
    cfg: Config,
    creds: Credentials,
    ip: IpAddr,
    profile: stream::VideoProfile,
    ctx: SessionCtx,
) {
    let SessionCtx {
        link,
        status,
        window,
        stop,
        state,
    } = ctx;
    // status text for the own capture window
    let st = |t: &str| {
        log::info!("{t}");
        *status.lock().unwrap() = t.to_string();
        if let Some(w) = *window.lock().unwrap() {
            w.refresh();
        }
    };
    let set_phase = |p: Phase| state.lock().unwrap().phase = p;
    let host_label = if cfg.console.nickname.is_empty() {
        ip.to_string()
    } else {
        format!("{} ({ip})", cfg.console.nickname)
    };
    let params = stream::StreamParams {
        ip,
        profile,
        dualsense: cfg.stream.dualsense,
    };
    let mut backoff = Duration::from_secs(2);
    // when our last live session ended; the PS5 holds it for a while afterwards
    let mut last_live_end: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        let attempt = (|| -> Result<stream::StreamEnd> {
            set_phase(Phase::Connecting);
            st(&format!("Connecting to {host_label} …"));
            // connect first; only wake when the console isn't listening, so a running
            // console connects without waiting on (UDP) discovery
            let nonce = match session::request_session(ip, &creds) {
                Ok(n) => n,
                Err(e) if cfg.console.wake && is_unreachable(&e) => {
                    log::info!("{e:#}");
                    set_phase(Phase::Waking);
                    wake_and_wait(ip, &creds, &stop, &st)?;
                    if stop.load(Ordering::Relaxed) {
                        return Ok(stream::StreamEnd::Stopped);
                    }
                    set_phase(Phase::Connecting);
                    st(&format!("Connecting to {host_label} …"));
                    session::request_session(ip, &creds)?
                }
                Err(e) => return Err(e),
            };
            set_phase(Phase::SigningIn);
            let ctrl = session::Ctrl::connect(
                ip,
                &nonce,
                &creds,
                cfg.console.login_pin.as_deref(),
                params.dualsense,
            )?;
            set_phase(Phase::StartingStream);
            st("Starting the controller connection …");
            let connected_status = format!(
                "Connected to {host_label} as {}",
                if params.dualsense {
                    "DualSense"
                } else {
                    "DualShock 4"
                }
            );
            let started = Instant::now();
            let done = AtomicBool::new(false);
            let end = thread::scope(|s| {
                s.spawn(|| {
                    // the stream marks the link as streaming once input is live
                    while !done.load(Ordering::Relaxed) {
                        if link.is_streaming() {
                            {
                                let mut sst = state.lock().unwrap();
                                sst.phase = Phase::Ready;
                                sst.problem = None;
                            }
                            st(&connected_status);
                            break;
                        }
                        thread::sleep(Duration::from_millis(50));
                    }
                });
                let r = stream::run(&params, &ctrl, &link, &stop);
                done.store(true, Ordering::Relaxed);
                r
            });
            if started.elapsed() > Duration::from_secs(30) {
                backoff = Duration::from_secs(2);
            }
            if link.is_streaming() || state.lock().unwrap().phase == Phase::Ready {
                last_live_end = Some(Instant::now());
            }
            end
        })();
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let problem = match attempt {
            Ok(stream::StreamEnd::Stopped) => break,
            Ok(stream::StreamEnd::RemoteDisconnected(r)) => Problem {
                kind: ProblemKind::Dropped,
                title: "The PS5 ended the connection".into(),
                fix: "This happens when the PS5 goes to rest mode or another device takes over. This app reconnects.".into(),
                detail: format!("console said: {r}"),
            },
            Ok(stream::StreamEnd::CtrlFailed) => Problem {
                kind: ProblemKind::Dropped,
                title: "The connection to the PS5 dropped".into(),
                fix: "This app reconnects.".into(),
                detail: "session control channel closed".into(),
            },
            Err(e) => explain(&e, &ip.to_string()),
        };
        let releasing = problem.kind == ProblemKind::InUse
            && last_live_end
                .map(|t| t.elapsed() < Duration::from_secs(60))
                .unwrap_or(false);
        let problem = if releasing {
            Problem {
                kind: ProblemKind::InUse,
                title: "The PS5 is still closing the previous connection".into(),
                fix: "This takes a few seconds after a dropped connection. Reconnecting automatically.".into(),
                detail: problem.detail,
            }
        } else {
            problem
        };
        log::warn!("{}: {}", problem.title, problem.detail);
        let retry = problem.retryable() && cfg.stream.reconnect;
        {
            let mut sst = state.lock().unwrap();
            sst.problem = Some(problem.clone());
            sst.phase = if retry {
                Phase::Retrying {
                    at: Instant::now() + backoff,
                }
            } else {
                Phase::Stopped
            };
        }
        if !retry {
            // nothing changes by itself: end the session so the settings window can help
            st(&problem.title);
            if let Some(w) = *window.lock().unwrap() {
                w.close();
            }
            break;
        }
        st(&format!(
            "{}\nTrying again in {}s …",
            problem.title,
            backoff.as_secs()
        ));
        let until = Instant::now() + backoff;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(100));
        }
        // waiting for the PS5 to release our own old session: poll steadily instead of backing off
        let just_dropped = problem.kind == ProblemKind::Dropped
            && last_live_end
                .map(|t| t.elapsed() < Duration::from_secs(10))
                .unwrap_or(false);
        backoff = if releasing || just_dropped {
            Duration::from_secs(2)
        } else {
            (backoff * 2).min(Duration::from_secs(30))
        };
    }
    state.lock().unwrap().phase = Phase::Stopped;
    *status.lock().unwrap() = "Stopped".into();
}

/// Connects and signs in once (waking the console if needed) without starting a stream
/// or grabbing input. Used by the setup wizard to test a fresh pairing.
pub fn check_connection(cfg: &Config) -> std::result::Result<(), Problem> {
    let r = (|| -> Result<()> {
        let creds = cfg.credentials()?;
        let ip = resolve_host(&cfg.console.host)?;
        let stop = AtomicBool::new(false);
        let nonce = match session::request_session(ip, &creds) {
            Ok(n) => n,
            Err(e) if cfg.console.wake && is_unreachable(&e) => {
                wake_and_wait(ip, &creds, &stop, &|_| {})?;
                session::request_session(ip, &creds)?
            }
            Err(e) => return Err(e),
        };
        // dropping it right away closes the session again
        let _ctrl =
            session::Ctrl::connect(ip, &nonce, &creds, cfg.console.login_pin.as_deref(), false)?;
        Ok(())
    })();
    r.map_err(|e| explain(&e, &cfg.console.host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problems_are_explained() {
        let e: anyhow::Error = session::LoginError::PasscodeNeeded.into();
        let p = explain(&e.context("ctrl"), "x");
        assert_eq!(p.kind, ProblemKind::PasscodeNeeded);
        assert!(!p.retryable());

        let e: anyhow::Error = session::SessionRejected {
            http: 403,
            reason: 0x80108b10,
        }
        .into();
        assert_eq!(explain(&e, "x").kind, ProblemKind::InUse);
        let e: anyhow::Error = session::SessionRejected {
            http: 403,
            reason: 0x80108b26,
        }
        .into();
        let p = explain(&e, "x");
        assert_eq!(p.kind, ProblemKind::NotPaired);
        assert!(!p.retryable());

        let io = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let e = anyhow::Error::new(io).context("connecting to 1.2.3.4:9295");
        let p = explain(&e, "1.2.3.4");
        assert_eq!(p.kind, ProblemKind::Unreachable);
        assert!(p.retryable());
        assert!(p.fix.contains("1.2.3.4"));

        // a failing stream socket is a dropped connection, not an unreachable console
        let io = std::io::Error::from_raw_os_error(10054);
        let e = anyhow::Error::new(io).context("Takion recv");
        assert_eq!(explain(&e, "x").kind, ProblemKind::Dropped);
    }
}
