//! Takion: the SCTP-like UDP transport of Remote Play. Only what an input-only client needs:
//! handshake, reliable data messages (with acks and resends), congestion reports and
//! encrypted feedback (controller) packets. Video and audio packets are dropped unparsed.
//! Port of the relevant parts of chiaki-ng `lib/src/takion.c` and `takionsendbuffer.c`.

use std::collections::BTreeMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use crate::gkcrypt::{GkCrypt, BLOCK_SIZE, GMAC_SIZE};

pub const PACKET_TYPE_CONTROL: u8 = 0;
pub const PACKET_TYPE_FEEDBACK_HISTORY: u8 = 1;
pub const PACKET_TYPE_CONGESTION: u8 = 5;
pub const PACKET_TYPE_FEEDBACK_STATE: u8 = 6;

const CHUNK_TYPE_DATA: u8 = 0;
const CHUNK_TYPE_INIT: u8 = 1;
const CHUNK_TYPE_INIT_ACK: u8 = 2;
const CHUNK_TYPE_DATA_ACK: u8 = 3;
const CHUNK_TYPE_COOKIE: u8 = 0xa;
const CHUNK_TYPE_COOKIE_ACK: u8 = 0xb;

const A_RWND: u32 = 0x19000;
const OUTBOUND_STREAMS: u16 = 0x64;
const INBOUND_STREAMS: u16 = 0x64;
const MESSAGE_HEADER_SIZE: usize = 0x10;
const COOKIE_SIZE: usize = 0x20;
const EXPECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TRIES: usize = 3;

const RESEND_TIMEOUT: Duration = Duration::from_millis(200);
const RESEND_TRIES_MAX: u32 = 25;

pub const DATA_TYPE_PROTOBUF: u8 = 0;

struct Unacked {
    packet: Vec<u8>,
    last_send: Instant,
    tries: u32,
}

struct SendState {
    key_pos: u64,
    seq_num: u32,
    crypt: Option<GkCrypt>,
    unacked: BTreeMap<u32, Unacked>,
}

pub struct Takion {
    sock: UdpSocket,
    tag_local: u32,
    tag_remote: u32,
    send: Mutex<SendState>,
    recv: Mutex<RecvState>,
    /// when the current run of unexpected receive errors began
    recv_error_since: Mutex<Option<Instant>>,
}

struct RecvState {
    /// next expected remote data sequence number
    expected_seq: u32,
    /// out of order data messages waiting for the gap to fill
    pending: BTreeMap<u32, (u16, Vec<u8>)>,
}

/// A reliable data message delivered by the console.
pub struct DataMessage {
    #[allow(dead_code)]
    pub channel: u16,
    pub data_type: u8,
    pub payload: Vec<u8>,
}

/// Receive errors that mean "nothing this time" rather than a broken connection.
/// Windows sometimes reports ERROR_IO_PENDING (997) instead of a timeout for a blocking recv
/// with a short SO_RCVTIMEO, and WSAEMSGSIZE (10040) for a datagram larger than the buffer.
fn is_transient(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
            | std::io::ErrorKind::Interrupted
    ) || matches!(e.raw_os_error(), Some(997) | Some(10040))
}

/// How long receive errors may go on before the connection counts as broken.
const RECV_ERROR_GRACE: Duration = Duration::from_secs(2);

fn seq_lt(a: u32, b: u32) -> bool {
    a != b && b.wrapping_sub(a) < 0x8000_0000
}

fn write_message_header(
    buf: &mut [u8],
    tag: u32,
    key_pos: u64,
    chunk_type: u8,
    chunk_flags: u8,
    payload_size: usize,
) {
    buf[0..4].copy_from_slice(&tag.to_be_bytes());
    buf[4..8].fill(0); // gmac
    buf[8..12].copy_from_slice(&(key_pos as u32).to_be_bytes());
    buf[12] = chunk_type;
    buf[13] = chunk_flags;
    buf[14..16].copy_from_slice(&((payload_size + 4) as u16).to_be_bytes());
}

struct ParsedMessage<'a> {
    tag: u32,
    chunk_type: u8,
    chunk_flags: u8,
    payload: &'a [u8],
}

/// Parses a message chunk (the packet without its leading type byte).
fn parse_message(buf: &[u8]) -> Option<ParsedMessage<'_>> {
    if buf.len() < MESSAGE_HEADER_SIZE {
        return None;
    }
    let tag = u32::from_be_bytes(buf[0..4].try_into().unwrap());
    let payload_size = u16::from_be_bytes(buf[14..16].try_into().unwrap()) as usize;
    if buf.len() != payload_size + 0xc {
        return None;
    }
    Some(ParsedMessage {
        tag,
        chunk_type: buf[12],
        chunk_flags: buf[13],
        payload: &buf[MESSAGE_HEADER_SIZE..],
    })
}

impl Takion {
    /// Opens the UDP socket to `addr` and performs the INIT / COOKIE handshake.
    pub fn connect(addr: SocketAddr) -> Result<Takion> {
        let bind: SocketAddr = if addr.is_ipv4() {
            "0.0.0.0:0".parse()?
        } else {
            "[::]:0".parse()?
        };
        let sock = UdpSocket::bind(bind).context("binding Takion socket")?;
        sock.connect(addr).context("connecting Takion socket")?;
        sys::set_recv_buffer(&sock, A_RWND as i32);

        let tag_local: u32 = loop {
            let t = rand::random::<u32>();
            if t != 0 {
                break t;
            }
        };

        sock.set_read_timeout(Some(EXPECT_TIMEOUT))?;

        // INIT ->  INIT_ACK <-
        let mut init = [0u8; 1 + MESSAGE_HEADER_SIZE + 0x10];
        init[0] = PACKET_TYPE_CONTROL;
        write_message_header(&mut init[1..], 0, 0, CHUNK_TYPE_INIT, 0, 0x10);
        {
            let pl = &mut init[1 + MESSAGE_HEADER_SIZE..];
            pl[0..4].copy_from_slice(&tag_local.to_be_bytes());
            pl[4..8].copy_from_slice(&A_RWND.to_be_bytes());
            pl[8..10].copy_from_slice(&OUTBOUND_STREAMS.to_be_bytes());
            pl[10..12].copy_from_slice(&INBOUND_STREAMS.to_be_bytes());
            pl[12..16].copy_from_slice(&tag_local.to_be_bytes()); // initial seq num
        }

        let mut init_ack = None;
        for attempt in 0..CONNECT_TRIES {
            if attempt > 0 {
                log::warn!("Takion: no INIT ACK yet, retrying ({})", attempt + 1);
            }
            sock.send(&init)?;
            match Self::recv_chunk(&sock, tag_local, CHUNK_TYPE_INIT_ACK) {
                Ok(p) => {
                    init_ack = Some(p);
                    break;
                }
                Err(e) => log::warn!("Takion: {e:#}"),
            }
        }
        let init_ack = init_ack.context("Takion handshake failed: no INIT ACK from console")?;
        if init_ack.len() < 0x10 + COOKIE_SIZE {
            bail!("Takion INIT ACK too short");
        }
        let tag_remote = u32::from_be_bytes(init_ack[0..4].try_into().unwrap());
        let out_streams = u16::from_be_bytes(init_ack[8..10].try_into().unwrap());
        let in_streams = u16::from_be_bytes(init_ack[10..12].try_into().unwrap());
        if tag_remote == 0 {
            bail!("Takion remote tag is 0");
        }
        if out_streams == 0
            || in_streams == 0
            || out_streams > INBOUND_STREAMS
            || in_streams < OUTBOUND_STREAMS
        {
            bail!("Takion stream count check failed");
        }
        let cookie = &init_ack[0x10..0x10 + COOKIE_SIZE];

        // COOKIE ->  COOKIE_ACK <-
        let mut cookie_msg = [0u8; 1 + MESSAGE_HEADER_SIZE + COOKIE_SIZE];
        cookie_msg[0] = PACKET_TYPE_CONTROL;
        write_message_header(
            &mut cookie_msg[1..],
            tag_remote,
            0,
            CHUNK_TYPE_COOKIE,
            0,
            COOKIE_SIZE,
        );
        cookie_msg[1 + MESSAGE_HEADER_SIZE..].copy_from_slice(cookie);
        let mut ok = false;
        for attempt in 0..CONNECT_TRIES {
            if attempt > 0 {
                log::warn!(
                    "Takion: no COOKIE ACK yet, resending cookie ({})",
                    attempt + 1
                );
            }
            sock.send(&cookie_msg)?;
            match Self::recv_chunk(&sock, tag_local, CHUNK_TYPE_COOKIE_ACK) {
                Ok(_) => {
                    ok = true;
                    break;
                }
                Err(e) => log::warn!("Takion: {e:#}"),
            }
        }
        if !ok {
            bail!("Takion handshake failed: no COOKIE ACK from console");
        }
        log::info!("Takion connected (remote tag {tag_remote:#x})");

        Ok(Takion {
            sock,
            tag_local,
            tag_remote,
            send: Mutex::new(SendState {
                key_pos: 0,
                seq_num: tag_local,
                crypt: None,
                unacked: BTreeMap::new(),
            }),
            recv: Mutex::new(RecvState {
                expected_seq: tag_remote,
                pending: BTreeMap::new(),
            }),
            recv_error_since: Mutex::new(None),
        })
    }

    /// Waits for a control chunk of `chunk_type`, skipping anything else (e.g. a repeated INIT ACK).
    fn recv_chunk(sock: &UdpSocket, tag_local: u32, chunk_type: u8) -> Result<Vec<u8>> {
        let deadline = Instant::now() + EXPECT_TIMEOUT;
        let mut buf = [0u8; 1500];
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                bail!("timed out waiting for chunk type {chunk_type:#x}");
            }
            sock.set_read_timeout(Some(left))?;
            let n = match sock.recv(&mut buf) {
                Ok(n) => n,
                Err(e) if is_transient(&e) => continue,
                Err(e) => return Err(e).context("Takion recv"),
            };
            if n < 1 || buf[0] != PACKET_TYPE_CONTROL {
                continue;
            }
            let Some(msg) = parse_message(&buf[1..n]) else {
                continue;
            };
            if msg.tag != tag_local || msg.chunk_type != chunk_type || msg.chunk_flags != 0 {
                continue;
            }
            return Ok(msg.payload.to_vec());
        }
    }

    pub fn set_crypt(&self, crypt: GkCrypt) {
        self.send.lock().unwrap().crypt = Some(crypt);
    }

    pub fn set_read_timeout(&self, t: Duration) -> Result<()> {
        self.sock.set_read_timeout(Some(t))?;
        Ok(())
    }

    fn advance_key_pos(st: &mut SendState, mut size: usize) -> u64 {
        size += size % BLOCK_SIZE;
        if st.crypt.is_some() {
            let cur = st.key_pos;
            st.key_pos += size as u64;
            cur
        } else {
            0
        }
    }

    /// Fills in the GMAC of a control or congestion packet.
    fn packet_mac(crypt: &mut Option<GkCrypt>, buf: &mut [u8], key_pos: u64) {
        let (mac_off, key_pos_off) = match buf[0] & 0xf {
            PACKET_TYPE_CONTROL => (5, 9),
            PACKET_TYPE_CONGESTION => (7, 0xb),
            _ => unreachable!("packet_mac on unsupported packet type"),
        };
        buf[mac_off..mac_off + GMAC_SIZE].fill(0);
        if let Some(c) = crypt {
            let mut saved = [0u8; 4];
            saved.copy_from_slice(&buf[key_pos_off..key_pos_off + 4]);
            buf[key_pos_off..key_pos_off + 4].fill(0);
            let mac = c.gmac(key_pos, buf);
            buf[key_pos_off..key_pos_off + 4].copy_from_slice(&saved);
            buf[mac_off..mac_off + GMAC_SIZE].copy_from_slice(&mac);
        }
    }

    /// Sends a reliable data message. `first` selects the 9 byte header (with data type)
    /// used for a message's first fragment; continuation fragments use 8 bytes.
    fn send_data_inner(
        &self,
        chunk_flags: u8,
        channel: u16,
        data: &[u8],
        first: bool,
    ) -> Result<()> {
        let mut st = self.send.lock().unwrap();
        let key_pos = Self::advance_key_pos(&mut st, data.len());
        let header = if first { 9 } else { 8 };
        let mut packet = vec![0u8; 1 + MESSAGE_HEADER_SIZE + header + data.len()];
        packet[0] = PACKET_TYPE_CONTROL;
        write_message_header(
            &mut packet[1..],
            self.tag_remote,
            key_pos,
            CHUNK_TYPE_DATA,
            chunk_flags,
            header + data.len(),
        );
        let seq = st.seq_num;
        st.seq_num = st.seq_num.wrapping_add(1);
        let pl = &mut packet[1 + MESSAGE_HEADER_SIZE..];
        pl[0..4].copy_from_slice(&seq.to_be_bytes());
        pl[4..6].copy_from_slice(&channel.to_be_bytes());
        pl[6..8].fill(0);
        if first {
            pl[8] = DATA_TYPE_PROTOBUF;
        }
        pl[header..].copy_from_slice(data);
        Self::packet_mac(&mut st.crypt, &mut packet, key_pos);
        self.sock.send(&packet).context("Takion send data")?;
        st.unacked.insert(
            seq,
            Unacked {
                packet,
                last_send: Instant::now(),
                tries: 0,
            },
        );
        Ok(())
    }

    pub fn send_data(&self, chunk_flags: u8, channel: u16, data: &[u8]) -> Result<()> {
        self.send_data_inner(chunk_flags, channel, data, true)
    }

    /// Sends a protobuf message that may exceed the MTU, split like chiaki-ng splits BIG.
    pub fn send_data_fragmented(&self, channel: u16, data: &[u8], mtu: usize) -> Result<()> {
        let mtu = mtu - 50;
        let mut pos = 0usize;
        let mut remaining = data.len();
        let mut first = true;
        while mtu < remaining + 26 || (mtu < remaining + 25 && !first) {
            let size = if first { mtu - 26 } else { mtu - 25 };
            self.send_data_inner(0, channel, &data[pos..pos + size], first)?;
            first = false;
            pos += size;
            remaining -= size;
        }
        if remaining > 0 {
            self.send_data_inner(1, channel, &data[pos..], first)?;
        }
        Ok(())
    }

    fn send_data_ack(&self, seq: u32) -> Result<()> {
        let mut st = self.send.lock().unwrap();
        let mut buf = [0u8; 1 + MESSAGE_HEADER_SIZE + 0xc];
        buf[0] = PACKET_TYPE_CONTROL;
        let key_pos = Self::advance_key_pos(&mut st, buf.len());
        write_message_header(
            &mut buf[1..],
            self.tag_remote,
            key_pos,
            CHUNK_TYPE_DATA_ACK,
            0,
            0xc,
        );
        let pl = &mut buf[1 + MESSAGE_HEADER_SIZE..];
        pl[0..4].copy_from_slice(&seq.to_be_bytes());
        pl[4..8].copy_from_slice(&A_RWND.to_be_bytes());
        Self::packet_mac(&mut st.crypt, &mut buf, key_pos);
        self.sock.send(&buf)?;
        Ok(())
    }

    pub fn send_congestion(&self, received: u16, lost: u16) -> Result<()> {
        let mut st = self.send.lock().unwrap();
        let mut buf = [0u8; 0xf];
        let key_pos = Self::advance_key_pos(&mut st, buf.len());
        buf[0] = PACKET_TYPE_CONGESTION;
        buf[3..5].copy_from_slice(&received.to_be_bytes());
        buf[5..7].copy_from_slice(&lost.to_be_bytes());
        buf[0xb..0xf].copy_from_slice(&(key_pos as u32).to_be_bytes());
        Self::packet_mac(&mut st.crypt, &mut buf, key_pos);
        self.sock.send(&buf)?;
        Ok(())
    }

    /// Encrypts, authenticates and sends a feedback packet whose 12 byte header is already set up.
    fn send_feedback_packet(&self, buf: &mut [u8]) -> Result<()> {
        let mut st = self.send.lock().unwrap();
        let payload_size = buf.len() - 0xc;
        let key_pos = Self::advance_key_pos(&mut st, payload_size + BLOCK_SIZE);
        let Some(crypt) = st.crypt.as_mut() else {
            bail!("feedback packet before crypt was set up");
        };
        crypt.crypt(key_pos + BLOCK_SIZE as u64, &mut buf[0xc..]);
        buf[4..8].copy_from_slice(&(key_pos as u32).to_be_bytes());
        buf[8..12].fill(0);
        let mac = crypt.gmac(key_pos, buf);
        buf[8..12].copy_from_slice(&mac);
        self.sock.send(buf)?;
        Ok(())
    }

    pub fn send_feedback_state(&self, seq: u16, payload: &[u8]) -> Result<()> {
        let mut buf = vec![0u8; 0xc + payload.len()];
        buf[0] = PACKET_TYPE_FEEDBACK_STATE;
        buf[1..3].copy_from_slice(&seq.to_be_bytes());
        buf[3] = 0;
        buf[0xc..].copy_from_slice(payload);
        self.send_feedback_packet(&mut buf)
    }

    pub fn send_feedback_history(&self, seq: u16, event_count: u8, payload: &[u8]) -> Result<()> {
        let mut buf = vec![0u8; 0xc + payload.len()];
        buf[0] = PACKET_TYPE_FEEDBACK_HISTORY;
        buf[1..3].copy_from_slice(&seq.to_be_bytes());
        // number of events less one: the console adds one back
        buf[3] = event_count.saturating_sub(1);
        buf[0xc..].copy_from_slice(payload);
        self.send_feedback_packet(&mut buf)
    }

    /// Resends reliable data the console hasn't acknowledged yet.
    pub fn resend_unacked(&self) {
        let mut st = self.send.lock().unwrap();
        let now = Instant::now();
        let mut dropped = Vec::new();
        for (&seq, u) in st.unacked.iter_mut() {
            if now.duration_since(u.last_send) < RESEND_TIMEOUT {
                continue;
            }
            if u.tries >= RESEND_TRIES_MAX {
                dropped.push(seq);
                continue;
            }
            u.tries += 1;
            u.last_send = now;
            let _ = self.sock.send(&u.packet);
        }
        for seq in dropped {
            log::warn!("Takion: giving up on unacknowledged data {seq:#x}");
            st.unacked.remove(&seq);
        }
    }

    /// Receives one datagram and handles transport-level chunks. Returns the reliable data
    /// messages that became deliverable in order. Times out per the socket read timeout.
    pub fn poll(&self, buf: &mut [u8]) -> Result<Vec<DataMessage>> {
        let n = match self.sock.recv(buf) {
            Ok(n) => {
                *self.recv_error_since.lock().unwrap() = None;
                n
            }
            Err(e) if is_transient(&e) => return Ok(Vec::new()),
            // Windows reports ICMP port unreachable as a reset on the next recv
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                bail!("console closed the stream port")
            }
            Err(e) => {
                // an unexpected error: only give up when it keeps happening
                let mut since = self.recv_error_since.lock().unwrap();
                let first = *since.get_or_insert_with(Instant::now);
                if first.elapsed() < RECV_ERROR_GRACE {
                    log::debug!("Takion recv error (ignored for now): {e}");
                    return Ok(Vec::new());
                }
                return Err(e).context("Takion recv");
            }
        };
        if n == 0 || buf[0] & 0xf != PACKET_TYPE_CONTROL {
            // video, audio and anything else: not needed for input-only streaming
            return Ok(Vec::new());
        }
        let Some(msg) = parse_message(&buf[1..n]) else {
            return Ok(Vec::new());
        };
        if msg.tag != self.tag_local {
            return Ok(Vec::new());
        }
        match msg.chunk_type {
            CHUNK_TYPE_DATA => self.handle_data(msg.payload),
            CHUNK_TYPE_DATA_ACK => {
                if msg.payload.len() >= 4 {
                    let cumulative = u32::from_be_bytes(msg.payload[0..4].try_into().unwrap());
                    let mut st = self.send.lock().unwrap();
                    st.unacked
                        .retain(|&seq, _| !(seq == cumulative || seq_lt(seq, cumulative)));
                }
                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn handle_data(&self, payload: &[u8]) -> Result<Vec<DataMessage>> {
        if payload.len() < 9 {
            return Ok(Vec::new());
        }
        let seq = u32::from_be_bytes(payload[0..4].try_into().unwrap());
        let channel = u16::from_be_bytes(payload[4..6].try_into().unwrap());
        let mut out = Vec::new();
        let ack_seq;
        {
            let mut rs = self.recv.lock().unwrap();
            if seq == rs.expected_seq {
                out.push(DataMessage {
                    channel,
                    data_type: payload[8],
                    payload: payload[9..].to_vec(),
                });
                rs.expected_seq = rs.expected_seq.wrapping_add(1);
                loop {
                    let next = rs.expected_seq;
                    let Some((ch, p)) = rs.pending.remove(&next) else {
                        break;
                    };
                    out.push(DataMessage {
                        channel: ch,
                        data_type: p[8],
                        payload: p[9..].to_vec(),
                    });
                    rs.expected_seq = rs.expected_seq.wrapping_add(1);
                }
            } else if seq_lt(rs.expected_seq, seq) && rs.pending.len() < 64 {
                rs.pending.insert(seq, (channel, payload.to_vec()));
            }
            // ack everything received in order so far (also re-acks duplicates)
            ack_seq = rs.expected_seq.wrapping_sub(1);
        }
        self.send_data_ack(ack_seq)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// chiaki-ng test/takion.c test_takion_format_congestion
    #[test]
    fn congestion_packet_mac_vector() {
        let handshake_key = [
            0x54, 0x65, 0x4c, 0x34, 0x5c, 0xac, 0x56, 0xb8, 0xea, 0xe6, 0x15, 0x2a, 0xde, 0x1c,
            0xe2, 0xe8,
        ];
        let ecdh_secret = [
            0x00, 0x34, 0xf8, 0x21, 0xc7, 0xd9, 0xde, 0xa9, 0xe9, 0x11, 0xca, 0x5a, 0xd6, 0x7d,
            0x11, 0xce, 0x4f, 0x02, 0xb1, 0xce, 0x1e, 0xe7, 0xc3, 0x8d, 0x54, 0x39, 0xfa, 0x64,
            0xe3, 0xdb, 0xd8, 0x0d,
        ];
        let mut crypt = Some(GkCrypt::new(2, &handshake_key, &ecdh_secret));
        let mut buf = [
            0x05, 0x00, 0x42, 0x00, 0x1a, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
            0xe5,
        ];
        Takion::packet_mac(&mut crypt, &mut buf, 0x1e5);
        assert_eq!(
            buf,
            [
                0x05, 0x00, 0x42, 0x00, 0x1a, 0x00, 0x0a, 0x64, 0x8a, 0x7c, 0x74, 0x00, 0x00, 0x01,
                0xe5
            ]
        );
    }

    /// Plays the console side of a Takion session over loopback.
    #[test]
    fn loopback_handshake_fragments_and_acks() {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let addr = server.local_addr().unwrap();
        const TAG_REMOTE: u32 = 0xabcd1234;

        let console = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            // INIT
            let (n, peer) = server.recv_from(&mut buf).unwrap();
            let m = parse_message(&buf[1..n]).unwrap();
            assert_eq!(m.chunk_type, CHUNK_TYPE_INIT);
            let tag_local = u32::from_be_bytes(m.payload[0..4].try_into().unwrap());
            let init_seq = u32::from_be_bytes(m.payload[12..16].try_into().unwrap());
            assert_eq!(tag_local, init_seq);
            // INIT_ACK
            let mut ack = vec![0u8; 1 + MESSAGE_HEADER_SIZE + 0x10 + COOKIE_SIZE];
            write_message_header(
                &mut ack[1..],
                tag_local,
                0,
                CHUNK_TYPE_INIT_ACK,
                0,
                0x10 + COOKIE_SIZE,
            );
            let pl = &mut ack[1 + MESSAGE_HEADER_SIZE..];
            pl[0..4].copy_from_slice(&TAG_REMOTE.to_be_bytes());
            pl[8..10].copy_from_slice(&0x64u16.to_be_bytes());
            pl[10..12].copy_from_slice(&0x64u16.to_be_bytes());
            pl[0x10..].fill(0x5a);
            server.send_to(&ack, peer).unwrap();
            // COOKIE
            let (n, _) = server.recv_from(&mut buf).unwrap();
            let m = parse_message(&buf[1..n]).unwrap();
            assert_eq!(m.tag, TAG_REMOTE);
            assert_eq!(m.chunk_type, CHUNK_TYPE_COOKIE);
            assert_eq!(m.payload, &[0x5a; COOKIE_SIZE]);
            let mut cack = [0u8; 1 + MESSAGE_HEADER_SIZE];
            write_message_header(&mut cack[1..], tag_local, 0, CHUNK_TYPE_COOKIE_ACK, 0, 0);
            server.send_to(&cack, peer).unwrap();

            // fragmented data from the client: reassemble and ack the last fragment
            let mut assembled = Vec::new();
            let mut last_seq;
            loop {
                let (n, _) = server.recv_from(&mut buf).unwrap();
                assert!(
                    n <= MTU_TEST - 50,
                    "fragment of {n} bytes exceeds the MTU budget"
                );
                let m = parse_message(&buf[1..n]).unwrap();
                assert_eq!(m.chunk_type, CHUNK_TYPE_DATA);
                let seq = u32::from_be_bytes(m.payload[0..4].try_into().unwrap());
                let header = if assembled.is_empty() { 9 } else { 8 };
                assembled.extend_from_slice(&m.payload[header..]);
                last_seq = seq;
                if m.chunk_flags == 1 {
                    break;
                }
            }
            let mut dack = [0u8; 1 + MESSAGE_HEADER_SIZE + 0xc];
            write_message_header(&mut dack[1..], tag_local, 0, CHUNK_TYPE_DATA_ACK, 0, 0xc);
            dack[1 + MESSAGE_HEADER_SIZE..][0..4].copy_from_slice(&last_seq.to_be_bytes());
            server.send_to(&dack, peer).unwrap();

            // two data messages to the client, out of order
            for seq in [TAG_REMOTE + 1, TAG_REMOTE] {
                let body = [0x08, (seq - TAG_REMOTE) as u8];
                let mut d = vec![0u8; 1 + MESSAGE_HEADER_SIZE + 9 + body.len()];
                write_message_header(
                    &mut d[1..],
                    tag_local,
                    0,
                    CHUNK_TYPE_DATA,
                    1,
                    9 + body.len(),
                );
                let pl = &mut d[1 + MESSAGE_HEADER_SIZE..];
                pl[0..4].copy_from_slice(&seq.to_be_bytes());
                pl[9..].copy_from_slice(&body);
                server.send_to(&d, peer).unwrap();
            }
            // the client acks cumulatively; the last ack must cover both
            let mut acked = 0;
            for _ in 0..2 {
                let (n, _) = server.recv_from(&mut buf).unwrap();
                let m = parse_message(&buf[1..n]).unwrap();
                assert_eq!(m.chunk_type, CHUNK_TYPE_DATA_ACK);
                acked = u32::from_be_bytes(m.payload[0..4].try_into().unwrap());
            }
            assert_eq!(acked, TAG_REMOTE + 1);
            assembled
        });

        const MTU_TEST: usize = 1454;
        let t = Takion::connect(addr).unwrap();
        t.set_read_timeout(Duration::from_millis(20)).unwrap();
        let data: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        t.send_data_fragmented(1, &data, MTU_TEST).unwrap();

        let mut buf = [0u8; 2048];
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while got.len() < 2 && Instant::now() < deadline {
            got.extend(t.poll(&mut buf).unwrap());
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].payload, [0x08, 0]);
        assert_eq!(got[1].payload, [0x08, 1]);
        assert!(
            t.send.lock().unwrap().unacked.is_empty(),
            "client data should be acknowledged"
        );
        assert_eq!(console.join().unwrap(), data);
    }

    #[test]
    fn message_header_roundtrip() {
        let mut buf = [0u8; MESSAGE_HEADER_SIZE + 12];
        write_message_header(&mut buf, 0x1234, 0, CHUNK_TYPE_DATA_ACK, 0, 12);
        let m = parse_message(&buf).unwrap();
        assert_eq!(m.tag, 0x1234);
        assert_eq!(m.chunk_type, CHUNK_TYPE_DATA_ACK);
        assert_eq!(m.payload.len(), 12);
    }
}

#[cfg(windows)]
mod sys {
    use std::os::windows::io::AsRawSocket;
    use windows_sys::Win32::Networking::WinSock::{setsockopt, SOCKET, SOL_SOCKET, SO_RCVBUF};

    pub fn set_recv_buffer(sock: &std::net::UdpSocket, size: i32) {
        unsafe {
            setsockopt(
                sock.as_raw_socket() as SOCKET,
                SOL_SOCKET,
                SO_RCVBUF,
                &size as *const i32 as *const u8,
                std::mem::size_of::<i32>() as i32,
            );
        }
    }
}

#[cfg(not(windows))]
mod sys {
    pub fn set_recv_buffer(_sock: &std::net::UdpSocket, _size: i32) {}
}

#[cfg(test)]
mod recv_error_tests {
    #[test]
    fn spurious_windows_errors_are_transient() {
        assert!(super::is_transient(&std::io::Error::from_raw_os_error(997)));
        assert!(super::is_transient(&std::io::Error::from_raw_os_error(
            10040
        )));
        assert!(super::is_transient(&std::io::Error::from(
            std::io::ErrorKind::TimedOut
        )));
        assert!(!super::is_transient(&std::io::Error::from(
            std::io::ErrorKind::ConnectionReset
        )));
    }
}
