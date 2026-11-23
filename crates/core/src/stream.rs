//! Stream connection (UDP port 9296) and the controller feedback sender.
//! Port of the input-relevant parts of chiaki-ng `lib/src/streamconnection.c`,
//! `feedbacksender.c` and `congestioncontrol.c`.

use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::controller::{self, ControllerState, HistoryEvent};
use crate::gkcrypt::{Ecdh, GkCrypt};
use crate::proto::{self, msg_type};
use crate::rpcrypt::RpCrypt;
use crate::session::Ctrl;
use crate::takion::{Takion, DATA_TYPE_PROTOBUF};

pub const STREAM_PORT: u16 = 9296;
const TAKION_VERSION_PS5: u32 = 12;
const EXPECT_TIMEOUT: Duration = Duration::from_secs(5);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const CONGESTION_INTERVAL: Duration = Duration::from_millis(200);
const POLL_TIMEOUT: Duration = Duration::from_millis(5);
// Senkusha (MTU/RTT probing) is skipped; these are chiaki-ng's fallback values.
const MTU: usize = 1454;
const RTT_MS: u32 = 1;

#[derive(Clone, Debug)]
pub struct VideoProfile {
    pub width: u32,
    pub height: u32,
    pub max_fps: u32,
    pub bitrate_kbps: u32,
}

impl VideoProfile {
    pub fn preset(resolution: &str, fps: u32) -> Result<Self> {
        let (width, height, bitrate_kbps) = match resolution {
            "360p" => (640, 360, 2000),
            "540p" => (960, 540, 6000),
            "720p" => (1280, 720, 10000),
            "1080p" => (1920, 1080, 15000),
            other => bail!("unknown resolution {other:?} (use 360p, 540p, 720p or 1080p)"),
        };
        if fps != 30 && fps != 60 {
            bail!("fps must be 30 or 60");
        }
        Ok(Self {
            width,
            height,
            max_fps: fps,
            bitrate_kbps,
        })
    }
}

fn launch_spec_json(profile: &VideoProfile, handshake_key: &[u8; 16]) -> String {
    format!(
        concat!(
            "{{",
            "\"sessionId\":\"sessionId4321\",",
            "\"streamResolutions\":[{{\"resolution\":{{\"width\":{},\"height\":{}}},\"maxFps\":{},\"score\":10}}],",
            "\"network\":{{\"bwKbpsSent\":{},\"bwLoss\":0.001000,\"mtu\":{},\"rtt\":{},\"ports\":[53,2053]}},",
            "\"slotId\":1,",
            "\"appSpecification\":{{\"minFps\":30,\"minBandwidth\":0,\"extTitleId\":\"ps3\",\"version\":1,",
            "\"timeLimit\":1,\"startTimeout\":100,\"afkTimeout\":100,\"afkTimeoutDisconnect\":100}},",
            "\"konan\":{{\"ps3AccessToken\":\"accessToken\",\"ps3RefreshToken\":\"refreshToken\"}},",
            "\"requestGameSpecification\":{{\"model\":\"bravia_tv\",\"platform\":\"android\",\"audioChannels\":\"5.1\",",
            "\"language\":\"sp\",\"acceptButton\":\"X\",\"connectedControllers\":[\"xinput\",\"ds3\",\"ds4\"],",
            "\"yuvCoefficient\":\"bt601\",\"videoEncoderProfile\":\"hw4.1\",\"audioEncoderProfile\":\"audio1\"",
            ",\"adaptiveStreamMode\": \"resize\"",
            "}},",
            "\"userProfile\":{{\"onlineId\":\"psnId\",\"npId\":\"npId\",\"region\":\"US\",\"languagesUsed\":[\"en\",\"jp\"]}},",
            "\"videoCodec\":\"avc\",",
            "\"dynamicRange\":\"SDR\",",
            "\"handshakeKey\":\"{}\"",
            "}}"
        ),
        profile.width,
        profile.height,
        profile.max_fps,
        profile.bitrate_kbps,
        MTU,
        RTT_MS,
        B64.encode(handshake_key)
    )
}

// ---------------------------------------------------------------------------------------------
// Feedback sender

const STATE_KEEPALIVE: Duration = Duration::from_millis(200);
const HISTORY_BUFFER_SIZE: usize = 0x10;
const HISTORY_RESEND_EVENT_COUNT: usize = 0x8;
/// After the newest history packet, it is repeated at these offsets if nothing new happened,
/// so a lost packet carrying a button release doesn't leave the button held.
const HISTORY_TAIL_MS: [u64; 4] = [20, 50, 100, 200];

struct FeedbackSender {
    takion: Arc<Takion>,
    state_prev_sent: ControllerState,
    history_prev: ControllerState,
    state_seq: u16,
    history_seq: u16,
    /// newest first
    events: VecDeque<HistoryEvent>,
    last_history: Option<(Vec<u8>, u8, u16)>,
    last_history_sent: Instant,
    tail_index: usize,
    last_state_sent: Instant,
}

impl FeedbackSender {
    fn new(takion: Arc<Takion>) -> Self {
        let idle = ControllerState::default();
        Self {
            takion,
            state_prev_sent: idle,
            history_prev: idle,
            state_seq: 0,
            history_seq: 0,
            events: VecDeque::new(),
            last_history: None,
            last_history_sent: Instant::now(),
            tail_index: HISTORY_TAIL_MS.len(),
            last_state_sent: Instant::now() - STATE_KEEPALIVE,
        }
    }

    fn send_state(&mut self, state: &ControllerState) {
        let payload = controller::format_feedback_state_v12(state);
        if let Err(e) = self.takion.send_feedback_state(self.state_seq, &payload) {
            log::warn!("sending feedback state failed: {e:#}");
        }
        self.state_seq = self.state_seq.wrapping_add(1);
        self.state_prev_sent = *state;
        self.last_state_sent = Instant::now();
    }

    fn send_history(&mut self, payload: &[u8], count: u8, seq: u16) {
        if let Err(e) = self.takion.send_feedback_history(seq, count, payload) {
            log::warn!("sending feedback history failed: {e:#}");
        }
    }

    fn update(&mut self, now: &ControllerState) {
        let mut new_events = Vec::new();
        controller::record_history(&self.history_prev, now, &mut new_events);
        self.history_prev = *now;
        for ev in new_events {
            // every event takes the next sequence number; a packet carries its newest one
            self.history_seq = self.history_seq.wrapping_add(1);
            self.events.push_front(ev);
            self.events.truncate(HISTORY_BUFFER_SIZE);
            let mut payload = Vec::with_capacity(self.events.len() * 5);
            for e in &self.events {
                payload.extend_from_slice(&e.buf[..e.len]);
            }
            let count = self.events.len() as u8;
            let seq = self.history_seq;
            self.send_history(&payload, count, seq);
            self.last_history = Some((payload, count, seq));
            self.last_history_sent = Instant::now();
            self.tail_index = 0;
            self.events.truncate(HISTORY_RESEND_EVENT_COUNT);
        }
        if now.feedback_state_differs(&self.state_prev_sent) {
            self.send_state(now);
        }
    }

    fn tick(&mut self, current: &ControllerState) {
        let now = Instant::now();
        if now.duration_since(self.last_state_sent) >= STATE_KEEPALIVE {
            self.send_state(current);
        }
        if self.tail_index < HISTORY_TAIL_MS.len()
            && now
                >= self.last_history_sent + Duration::from_millis(HISTORY_TAIL_MS[self.tail_index])
        {
            self.tail_index += 1;
            if let Some((payload, count, seq)) = self.last_history.clone() {
                self.send_history(&payload, count, seq);
            }
        }
    }
}

/// Shared link between the input thread and the stream: holds the current controller state
/// and, while a stream is up, sends every change immediately.
pub struct ControllerLink {
    inner: Mutex<LinkInner>,
}

struct LinkInner {
    state: ControllerState,
    sender: Option<FeedbackSender>,
}

impl ControllerLink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(LinkInner {
                state: ControllerState::default(),
                sender: None,
            }),
        })
    }

    pub fn set_state(&self, state: &ControllerState) {
        let mut inner = self.inner.lock().unwrap();
        if inner.state == *state {
            return;
        }
        inner.state = *state;
        if let Some(s) = inner.sender.as_mut() {
            s.update(state);
        }
    }

    /// The controller state as last set (for the live preview).
    pub fn state(&self) -> ControllerState {
        self.inner.lock().unwrap().state
    }

    pub fn is_streaming(&self) -> bool {
        self.inner.lock().unwrap().sender.is_some()
    }

    fn attach(&self, takion: Arc<Takion>) {
        let mut inner = self.inner.lock().unwrap();
        let mut sender = FeedbackSender::new(takion);
        let state = inner.state;
        sender.update(&state);
        sender.tick(&state);
        inner.sender = Some(sender);
    }

    fn detach(&self) {
        self.inner.lock().unwrap().sender = None;
    }

    fn tick(&self) {
        let mut inner = self.inner.lock().unwrap();
        let state = inner.state;
        if let Some(s) = inner.sender.as_mut() {
            s.tick(&state);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Stream connection

pub struct StreamParams {
    pub ip: IpAddr,
    pub profile: VideoProfile,
    pub dualsense: bool,
}

#[derive(PartialEq)]
enum Phase {
    ExpectBang,
    ExpectStreamInfo,
    Running,
}

pub enum StreamEnd {
    Stopped,
    RemoteDisconnected(String),
    CtrlFailed,
}

fn big_message(
    ctrl: &Ctrl,
    crypt: &RpCrypt,
    profile: &VideoProfile,
    handshake_key: &[u8; 16],
    ecdh: &Ecdh,
) -> Vec<u8> {
    let json = launch_spec_json(profile, handshake_key);
    log::debug!("launch spec: {json}");
    let mut plain = json.into_bytes();
    plain.push(0); // trailing NUL is part of it
                   // the launch spec is XORed with the RP crypt stream of zeros
    let mut enc = vec![0u8; plain.len()];
    crypt.encrypt(0, &mut enc);
    for (e, p) in enc.iter_mut().zip(&plain) {
        *e ^= p;
    }
    let launch_spec_b64 = B64.encode(&enc);
    let (pub_key, sig) = ecdh.local_pub_key(handshake_key);

    let big = proto::Writer::new()
        .uint(1, TAKION_VERSION_PS5 as u64)
        .bytes(2, ctrl.session_id.as_bytes())
        .bytes(3, launch_spec_b64.as_bytes())
        .bytes(4, &[0, 0, 0, 0])
        .bytes(5, &pub_key)
        .bytes(6, &sig)
        .finish();
    proto::Writer::new()
        .uint(1, msg_type::BIG)
        .bytes(2, &big)
        .finish()
}

fn controller_connection_message(dualsense: bool) -> Vec<u8> {
    let payload = proto::Writer::new()
        .uint(2, 1) // connected
        .uint(3, if dualsense { 6 } else { 2 }) // DUALSENSE / DUALSHOCK4
        .finish();
    proto::Writer::new()
        .uint(1, msg_type::CONTROLLERCONNECTION)
        .bytes(22, &payload)
        .finish()
}

/// Stream info for the (unused) microphone, sent like chiaki-ng does after the console's stream info.
fn mic_stream_info_message() -> Vec<u8> {
    let mut header = [0u8; 14];
    header[0] = 1; // "bits" slot (chiaki-ng swaps the arguments)
    header[1] = 16; // "channels" slot
    header[2..6].copy_from_slice(&48000u32.to_be_bytes());
    header[6..10].copy_from_slice(&480u32.to_be_bytes());
    header[10..14].copy_from_slice(&1u32.to_be_bytes());
    let payload = proto::Writer::new().bytes(2, &header).finish();
    proto::Writer::new()
        .uint(1, msg_type::STREAMINFO)
        .bytes(15, &payload)
        .finish()
}

fn disconnect_message() -> Vec<u8> {
    let payload = proto::Writer::new()
        .bytes(1, b"Client Disconnecting")
        .finish();
    proto::Writer::new()
        .uint(1, msg_type::DISCONNECT)
        .bytes(10, &payload)
        .finish()
}

fn disconnect_reason(fields: &[(u32, proto::Value<'_>)]) -> String {
    proto::get_bytes(fields, 10)
        .map(|p| {
            let f = proto::fields(p);
            proto::get_bytes(&f, 1)
                .map(|r| String::from_utf8_lossy(r).to_string())
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Runs a stream connection until stopped or disconnected. While running, the
/// `ControllerLink` forwards controller state to the console.
pub fn run(
    params: &StreamParams,
    ctrl: &Ctrl,
    link: &ControllerLink,
    stop: &AtomicBool,
) -> Result<StreamEnd> {
    let addr = SocketAddr::new(params.ip, STREAM_PORT);
    let takion = Arc::new(Takion::connect(addr).context("stream connection")?);
    takion.set_read_timeout(POLL_TIMEOUT)?;

    let mut handshake_key = [0u8; 16];
    rand::Rng::fill(&mut rand::thread_rng(), &mut handshake_key);
    let ecdh = Ecdh::new();

    log::info!("sending BIG");
    let big = big_message(ctrl, &ctrl.crypt, &params.profile, &handshake_key, &ecdh);
    takion.send_data_fragmented(1, &big, MTU)?;

    let mut phase = Phase::ExpectBang;
    let mut phase_deadline = Instant::now() + EXPECT_TIMEOUT;
    let mut early_streaminfo: Option<Vec<u8>> = None;
    let mut last_congestion = Instant::now();
    let mut last_heartbeat = Instant::now();
    let mut buf = vec![0u8; 2048];

    let result = (|| -> Result<StreamEnd> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(StreamEnd::Stopped);
            }
            if ctrl.failed() {
                return Ok(StreamEnd::CtrlFailed);
            }
            let now = Instant::now();
            if phase != Phase::Running && now > phase_deadline {
                bail!(if phase == Phase::ExpectBang {
                    "timed out waiting for BANG from the console"
                } else {
                    "timed out waiting for stream info from the console"
                });
            }
            if now.duration_since(last_congestion) >= CONGESTION_INTERVAL {
                last_congestion = now;
                // nothing received means nothing lost: we never look at the video
                let _ = takion.send_congestion(0, 0);
            }
            if phase == Phase::Running {
                if now.duration_since(last_heartbeat) >= HEARTBEAT_INTERVAL {
                    last_heartbeat = now;
                    takion.send_data(1, 1, &proto::simple(msg_type::HEARTBEAT))?;
                }
                link.tick();
            }
            takion.resend_unacked();

            let mut messages = takion.poll(&mut buf)?;
            if phase == Phase::ExpectStreamInfo {
                if let Some(early) = early_streaminfo.take() {
                    messages.insert(
                        0,
                        crate::takion::DataMessage {
                            channel: 0,
                            data_type: DATA_TYPE_PROTOBUF,
                            payload: early,
                        },
                    );
                }
            }
            for m in messages {
                if m.data_type != DATA_TYPE_PROTOBUF {
                    continue; // rumble, pad info, trigger effects
                }
                let fields = proto::fields(&m.payload);
                let Some(t) = proto::get_varint(&fields, 1) else {
                    continue;
                };
                if t == msg_type::DISCONNECT {
                    let reason = disconnect_reason(&fields);
                    log::info!("console disconnected: {reason:?}");
                    return Ok(StreamEnd::RemoteDisconnected(reason));
                }
                match phase {
                    Phase::ExpectBang => {
                        if t == msg_type::STREAMINFO {
                            log::info!("stream info arrived before BANG, keeping it");
                            early_streaminfo = Some(m.payload.clone());
                            continue;
                        }
                        if t != msg_type::BANG {
                            log::warn!("expected BANG, got message type {t}");
                            continue;
                        }
                        let bang = proto::get_bytes(&fields, 3).context("BANG without payload")?;
                        let bf = proto::fields(bang);
                        if proto::get_varint(&bf, 4) != Some(1) {
                            bail!("console did not accept the protocol version");
                        }
                        if proto::get_varint(&bf, 3) != Some(1) {
                            bail!("console did not accept the encrypted key");
                        }
                        let remote_key =
                            proto::get_bytes(&bf, 8).context("BANG without ECDH public key")?;
                        let secret = ecdh.derive_secret(remote_key)?;
                        takion.set_crypt(GkCrypt::new(2, &handshake_key, &secret));
                        log::info!("BANG received, stream encryption set up");
                        phase = Phase::ExpectStreamInfo;
                        phase_deadline = Instant::now() + EXPECT_TIMEOUT;
                    }
                    Phase::ExpectStreamInfo => {
                        if t != msg_type::STREAMINFO {
                            log::debug!("expected stream info, got message type {t}");
                            continue;
                        }
                        log::info!("stream info received");
                        takion.send_data(1, 9, &proto::simple(msg_type::STREAMINFOACK))?;
                        takion.send_data(1, 1, &controller_connection_message(params.dualsense))?;
                        takion.send_data(1, 1, &mic_stream_info_message())?;
                        link.attach(takion.clone());
                        phase = Phase::Running;
                        last_heartbeat = Instant::now();
                        log::info!("connected: controller input is live");
                    }
                    Phase::Running => log::trace!("message type {t}"),
                }
            }
        }
    })();

    link.detach();
    let _ = takion.send_data(1, 1, &disconnect_message());
    result
}
