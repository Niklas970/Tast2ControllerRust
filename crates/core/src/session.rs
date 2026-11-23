//! Session request and the ctrl channel (TCP port 9295).
//! Port of the PS5 parts of chiaki-ng `lib/src/session.c` and `lib/src/ctrl.c`.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::config::Credentials;
use crate::http;
use crate::rpcrypt::RpCrypt;

pub const SESSION_PORT: u16 = 9295;
const TIMEOUT: Duration = Duration::from_secs(5);
const OS_TYPE: &[u8] = b"Win10.0.0\0";

const CTRL_SESSION_ID: u16 = 0x33;
const CTRL_HEARTBEAT_REQ: u16 = 0xfe;
const CTRL_HEARTBEAT_REP: u16 = 0x1fe;
const CTRL_LOGIN_PIN_REQ: u16 = 0x4;
const CTRL_LOGIN_PIN_REP: u16 = 0x8004;
const CTRL_LOGIN: u16 = 0x5;
const CTRL_GOTO_BED: u16 = 0x50;
const CTRL_ENABLE_DUALSENSE_FEATURES: u16 = 0x13;
const CTRL_MIC_TOGGLE: u16 = 0x36;
const CTRL_DISPLAY_DEVICES: u16 = 0x910;

fn application_reason(reason: u32) -> &'static str {
    match reason {
        0x80108b09 => "registration failed, probably an invalid PIN",
        0x80108b02 => "invalid PSN ID",
        0x80108b10 => "Remote Play on the console is already in use",
        0x80108b15 => "Remote Play on the console crashed",
        0x80108b11 => "RP-Version mismatch",
        _ => "unknown",
    }
}

/// The console answered the session request but refused it.
#[derive(Debug)]
pub struct SessionRejected {
    pub http: u16,
    pub reason: u32,
}

impl std::fmt::Display for SessionRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "session request rejected (HTTP {}, reason {:#x}: {})",
            self.http,
            self.reason,
            application_reason(self.reason)
        )
    }
}

impl std::error::Error for SessionRejected {}

impl SessionRejected {
    pub fn in_use(&self) -> bool {
        self.reason == 0x80108b10
    }
}

/// The console wants the PS5 user's login passcode.
#[derive(Debug, PartialEq, Eq)]
pub enum LoginError {
    PasscodeNeeded,
    PasscodeWrong,
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoginError::PasscodeNeeded => write!(f, "the console asked for the PS5 login passcode"),
            LoginError::PasscodeWrong => write!(f, "the console rejected the login passcode"),
        }
    }
}

impl std::error::Error for LoginError {}

fn connect_tcp(ip: IpAddr) -> Result<TcpStream> {
    let addr = SocketAddr::new(ip, SESSION_PORT);
    let s = TcpStream::connect_timeout(&addr, TIMEOUT).with_context(|| {
        format!("connecting to {addr} (is the console on and Remote Play enabled?)")
    })?;
    s.set_read_timeout(Some(TIMEOUT))?;
    s.set_write_timeout(Some(TIMEOUT))?;
    s.set_nodelay(true)?;
    Ok(s)
}

/// `GET /sie/ps5/rp/sess/init`, returns the session nonce.
pub fn request_session(ip: IpAddr, creds: &Credentials) -> Result<[u8; 16]> {
    let mut s = connect_tcp(ip)?;
    let req = format!(
        "GET /sie/ps5/rp/sess/init HTTP/1.1\r\n\
         Host: {ip}:{SESSION_PORT}\r\n\
         User-Agent: remoteplay Windows\r\n\
         Connection: close\r\n\
         Content-Length: 0\r\n\
         RP-Registkey: {}\r\n\
         Rp-Version: 1.0\r\n\
         \r\n",
        http::hex(creds.regist_key_trimmed())
    );
    s.write_all(req.as_bytes())?;
    let resp = http::recv_response(&mut s).context("session request")?;
    if resp.code != 200 {
        let reason = resp
            .header("RP-Application-Reason")
            .and_then(|r| u32::from_str_radix(r.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        return Err(SessionRejected {
            http: resp.code,
            reason,
        }
        .into());
    }
    let nonce_b64 = resp
        .header("RP-Nonce")
        .context("session response has no RP-Nonce")?;
    let nonce = B64.decode(nonce_b64.trim()).context("invalid RP-Nonce")?;
    nonce
        .try_into()
        .map_err(|_| anyhow!("RP-Nonce has the wrong size"))
}

struct CtrlWriter {
    stream: TcpStream,
    crypt: RpCrypt,
    counter: u64,
}

impl CtrlWriter {
    fn send(&mut self, msg_type: u16, payload: &[u8]) -> Result<()> {
        let mut buf = Vec::with_capacity(8 + payload.len());
        buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        buf.extend_from_slice(&msg_type.to_be_bytes());
        buf.extend_from_slice(&[0, 0]);
        if !payload.is_empty() {
            let mut enc = payload.to_vec();
            self.crypt.encrypt(self.counter, &mut enc);
            self.counter += 1;
            buf.extend_from_slice(&enc);
        }
        self.stream.write_all(&buf).context("ctrl send")?;
        Ok(())
    }
}

/// The running ctrl channel. Dropping it closes the connection, which ends the session.
pub struct Ctrl {
    writer: Arc<Mutex<CtrlWriter>>,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    pub session_id: String,
    pub crypt: RpCrypt,
}

impl Ctrl {
    pub fn connect(
        ip: IpAddr,
        nonce: &[u8; 16],
        creds: &Credentials,
        login_pin: Option<&str>,
        dualsense: bool,
    ) -> Result<Ctrl> {
        let crypt = RpCrypt::new_auth(nonce, &creds.morning);
        let mut counter_local = 0u64;
        let mut enc = |data: &[u8]| {
            let mut d = data.to_vec();
            crypt.encrypt(counter_local, &mut d);
            counter_local += 1;
            B64.encode(d)
        };

        let mut did = [0u8; 32];
        did[..10].copy_from_slice(&[0x00, 0x18, 0x00, 0x00, 0x00, 0x07, 0x00, 0x40, 0x00, 0x80]);
        rand::Rng::fill(&mut rand::thread_rng(), &mut did[10..26]);

        let auth = enc(&creds.regist_key);
        let did_b64 = enc(&did);
        let ostype = enc(OS_TYPE);
        let bitrate = enc(&[0u8; 4]);
        let streaming_type = enc(&1u32.to_le_bytes()); // 1 = H.264

        let req = format!(
            "GET /sie/ps5/rp/sess/ctrl HTTP/1.1\r\n\
             Host: {ip}:{SESSION_PORT}\r\n\
             User-Agent: remoteplay Windows\r\n\
             Connection: keep-alive\r\n\
             Content-Length: 0\r\n\
             RP-Auth: {auth}\r\n\
             RP-Version: 1.0\r\n\
             RP-Did: {did_b64}\r\n\
             RP-ControllerType: 3\r\n\
             RP-ClientType: 11\r\n\
             RP-OSType: {ostype}\r\n\
             RP-ConPath: 1\r\n\
             RP-PadProcNo: 3\r\n\
             RP-StartBitrate: {bitrate}\r\n\
             RP-StreamingType: {streaming_type}\r\n\
             \r\n"
        );

        let mut resp = None;
        for attempt in 0..2 {
            let mut s = connect_tcp(ip)?;
            s.write_all(req.as_bytes())?;
            match http::recv_response(&mut s) {
                Ok(r) => {
                    resp = Some((s, r));
                    break;
                }
                Err(e) if attempt == 0 => log::info!("ctrl request failed ({e:#}), retrying"),
                Err(e) => return Err(e).context("ctrl request"),
            }
        }
        let (stream, resp) = resp.unwrap();
        if resp.code != 200 {
            bail!("ctrl request rejected with HTTP {}", resp.code);
        }
        let mut counter_remote = 0u64;
        if let Some(st) = resp.header("RP-Server-Type") {
            if let Ok(mut v) = B64.decode(st.trim()) {
                if v.len() == 16 {
                    crypt.decrypt(counter_remote, &mut v);
                    counter_remote += 1;
                    log::info!("console server type: {} (2 = PS5)", v[0]);
                }
            }
        }
        if resp
            .header("RP-Prohibit")
            .map(|v| v.trim() == "1")
            .unwrap_or(false)
        {
            log::warn!(
                "console reports RP-Prohibit (streaming of the current content may be blocked)"
            );
        }

        stream.set_read_timeout(Some(Duration::from_millis(200)))?;
        let writer = Arc::new(Mutex::new(CtrlWriter {
            stream: stream.try_clone()?,
            crypt: crypt.clone(),
            counter: counter_local,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let (session_tx, session_rx) = mpsc::channel::<Result<Option<String>>>();

        let thread = {
            let writer = writer.clone();
            let stop = stop.clone();
            let failed = failed.clone();
            let crypt = crypt.clone();
            let login_pin = login_pin.map(|s| s.to_string());
            let mut reader = stream;
            let mut buf = resp.rest;
            thread::Builder::new().name("ctrl".into()).spawn(move || {
                let mut session_tx = Some(session_tx);
                let mut chunk = [0u8; 4096];
                let mut counter_remote = counter_remote;
                let mut pin_sent = false;
                'outer: while !stop.load(Ordering::Relaxed) {
                    while buf.len() >= 8 {
                        let size = u32::from_be_bytes(buf[0..4].try_into().unwrap()) as usize;
                        if buf.len() < 8 + size {
                            break;
                        }
                        let msg_type = u16::from_be_bytes(buf[4..6].try_into().unwrap());
                        let mut payload = buf[8..8 + size].to_vec();
                        buf.drain(..8 + size);
                        if !payload.is_empty() {
                            crypt.decrypt(counter_remote, &mut payload);
                            counter_remote += 1;
                        }
                        let mut w = writer.lock().unwrap();
                        match msg_type {
                            CTRL_SESSION_ID => {
                                let id = match parse_session_id(&payload) {
                                    Ok(id) => {
                                        log::info!("ctrl received session id");
                                        log::debug!("session id {id}");
                                        Some(id)
                                    }
                                    Err(e) => {
                                        log::warn!("{e:#}; using a fallback session id");
                                        None
                                    }
                                };
                                // enable features like chiaki-ng does (minus the on-screen keyboard)
                                if dualsense {
                                    let _ =
                                        w.send(CTRL_ENABLE_DUALSENSE_FEATURES, &[0x00, 0x40, 0x00]);
                                    let connect = [
                                        0xa0, 0xab, 0x51, 0xbd, 0xd1, 0x7e, 0x00, 0x00, 0xff, 0xff,
                                        0xff, 0xff, 0xff, 0x00, 0x00, 0x00,
                                    ];
                                    let _ = w.send(0x11, &connect);
                                }
                                let _ = w.send(CTRL_MIC_TOGGLE, &[0, 1, 1, 89]);
                                let _ = w.send(CTRL_MIC_TOGGLE, &[0, 1, 1, 89]);
                                let _ = w.send(CTRL_DISPLAY_DEVICES, &[0, 0, 0, 0]);
                                if let Some(tx) = session_tx.take() {
                                    let _ = tx.send(Ok(id));
                                }
                            }
                            CTRL_HEARTBEAT_REQ => {
                                log::debug!("ctrl heartbeat");
                                let _ = w.send(CTRL_HEARTBEAT_REP, &[]);
                            }
                            CTRL_LOGIN_PIN_REQ => {
                                match (&login_pin, pin_sent) {
                                    (Some(pin), false) => {
                                        log::info!(
                                            "console asked for the login passcode, sending it"
                                        );
                                        let _ = w.send(CTRL_LOGIN_PIN_REP, pin.as_bytes());
                                        pin_sent = true;
                                    }
                                    (_, sent) => {
                                        // asked again after we sent one: it was wrong
                                        let e = if sent {
                                            LoginError::PasscodeWrong
                                        } else {
                                            LoginError::PasscodeNeeded
                                        };
                                        if let Some(tx) = session_tx.take() {
                                            let _ = tx.send(Err(e.into()));
                                        }
                                        failed.store(true, Ordering::Relaxed);
                                        break 'outer;
                                    }
                                }
                            }
                            CTRL_LOGIN => {
                                if payload.first() == Some(&1) {
                                    if let Some(tx) = session_tx.take() {
                                        let _ = tx.send(Err(LoginError::PasscodeWrong.into()));
                                    }
                                    failed.store(true, Ordering::Relaxed);
                                    break 'outer;
                                }
                            }
                            other => {
                                log::debug!("ctrl message {other:#x} ({} bytes)", payload.len())
                            }
                        }
                    }
                    match reader.read(&mut chunk) {
                        Ok(0) => {
                            log::warn!("ctrl connection closed by the console");
                            failed.store(true, Ordering::Relaxed);
                            break;
                        }
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) => {}
                        Err(e) => {
                            if !stop.load(Ordering::Relaxed) {
                                log::warn!("ctrl receive failed: {e}");
                                failed.store(true, Ordering::Relaxed);
                            }
                            break;
                        }
                    }
                }
                if let Some(tx) = session_tx.take() {
                    let _ = tx.send(Err(anyhow!(
                        "ctrl channel closed before a session id arrived"
                    )));
                }
            })?
        };

        let mut ctrl = Ctrl {
            writer,
            stop,
            failed,
            thread: Some(thread),
            session_id: String::new(),
            crypt,
        };
        ctrl.session_id = match session_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(r) => r?.unwrap_or_else(fallback_session_id),
            Err(_) => {
                log::warn!("no session id from the console; using a fallback session id");
                fallback_session_id()
            }
        };
        Ok(ctrl)
    }

    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    /// Puts the console into rest mode.
    pub fn goto_bed(&self) -> Result<()> {
        self.writer.lock().unwrap().send(CTRL_GOTO_BED, &[])
    }
}

impl Drop for Ctrl {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(w) = self.writer.lock() {
            let _ = w.stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn parse_session_id(payload: &[u8]) -> Result<String> {
    if payload.len() < 2 {
        bail!("received session id is too short");
    }
    let id = &payload[1..]; // first byte is a size/marker (0x4a)
    if id.len() >= 79 {
        bail!("received session id is too long");
    }
    if id.len() < 24 {
        bail!("received session id is too short");
    }
    if !id.iter().all(|c| c.is_ascii_alphanumeric()) {
        bail!("received session id is invalid");
    }
    Ok(String::from_utf8_lossy(id).to_string())
}

fn fallback_session_id() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut rand_bytes = [0u8; 48];
    rand::Rng::fill(&mut rand::thread_rng(), &mut rand_bytes[..]);
    format!("{secs}{}", B64.encode(rand_bytes))
}
