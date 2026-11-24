//! Registering (pairing) with a PS5 using the 8 digit code from
//! Settings > System > Remote Play > Link Device, and waking a console from rest mode.
//! Port of the PS5 parts of chiaki-ng `lib/src/regist.c` and `lib/src/discovery.c`.

use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use crate::config::Credentials;
use crate::http;
use crate::rpcrypt::{aeropause, RpCrypt};
use crate::session::SESSION_PORT;

const CLIENT_TYPE: &str = "dabfa2ec873de5839bee8d3f4c0239c4282c07c25c6077a2931afcf0adc0d34f";
const TIMEOUT: Duration = Duration::from_secs(3);

pub struct Registered {
    pub creds: Credentials,
    pub nickname: String,
    pub mac: String,
}

/// `account_id` is the 8 byte PSN account id (little endian, as chiaki-ng expects it).
pub fn register(ip: IpAddr, pin: u32, account_id: [u8; 8]) -> Result<Registered> {
    // the console must answer a UDP search on the registration port first
    let udp = UdpSocket::bind(if ip.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    udp.connect(SocketAddr::new(ip, SESSION_PORT))?;
    udp.send(b"SRC3\0")?;
    udp.set_read_timeout(Some(TIMEOUT))?;
    let deadline = Instant::now() + TIMEOUT;
    let mut buf = [0u8; 256];
    loop {
        if Instant::now() > deadline {
            bail!("no answer to the registration search; keep the console's Link Device screen with the code open");
        }
        match udp.recv(&mut buf) {
            Ok(n) if n >= 4 && &buf[..4] == b"RES3" => break,
            Ok(_) => continue,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {
                bail!("the console refused the registration search; keep its Link Device screen with the code open")
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue
            }
            Err(e) => return Err(e).context("registration search"),
        }
    }
    std::thread::sleep(Duration::from_millis(100));

    // payload
    let mut ambassador = [0u8; 16];
    rand::Rng::fill(&mut rand::thread_rng(), &mut ambassador);
    const INNER_HEADER_OFF: usize = 0x1e0;
    let mut payload = vec![b'A'; INNER_HEADER_OFF];
    let key_0_off = (payload[0x18d] & 0x1f) as usize;
    let key_1_off = (payload[0] >> 3) as usize;
    let crypt = RpCrypt::new_regist(&ambassador, key_0_off, pin);
    let aero = aeropause(key_1_off, crypt.ambassador());
    payload[0xc7..0xcf].copy_from_slice(&aero[8..16]);
    payload[0x191..0x199].copy_from_slice(&aero[0..8]);
    let mut inner = format!(
        "Client-Type: {CLIENT_TYPE}\r\nNp-AccountId: {}\r\n",
        B64.encode(account_id)
    )
    .into_bytes();
    crypt.encrypt(0, &mut inner);
    payload.extend_from_slice(&inner);

    let header = format!(
        "POST /sie/ps5/rp/sess/rgst HTTP/1.1\r\n HTTP/1.1\r\n\
         HOST: 10.0.2.15\r\n\
         User-Agent: remoteplay Windows\r\n\
         Connection: close\r\n\
         Content-Length: {}\r\n\
         RP-Version: 1.0\r\n\
         \r\n",
        payload.len()
    );

    let mut tcp = TcpStream::connect_timeout(&SocketAddr::new(ip, SESSION_PORT), TIMEOUT)
        .context("connecting for registration")?;
    tcp.set_read_timeout(Some(TIMEOUT))?;
    tcp.set_write_timeout(Some(TIMEOUT))?;
    tcp.write_all(header.as_bytes())?;
    tcp.write_all(&payload)?;

    let resp = http::recv_response(&mut tcp).context("registration response")?;
    if resp.code != 200 {
        let reason = resp.header("RP-Application-Reason").unwrap_or("?");
        bail!("registration rejected with HTTP {} (reason {reason}); check the code and the account id", resp.code);
    }
    let content_len: usize = resp
        .header("Content-Length")
        .and_then(|v| v.trim().parse().ok())
        .filter(|&n| n > 0)
        .context("registration response has no Content-Length")?;
    let mut body = resp.rest;
    let mut chunk = [0u8; 1024];
    while body.len() < content_len {
        let n = tcp
            .read(&mut chunk)
            .context("receiving registration response")?;
        if n == 0 {
            bail!("connection closed during registration response");
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(content_len);
    crypt.decrypt(0, &mut body);
    let text = String::from_utf8_lossy(&body).to_string();
    let headers = http::parse_headers(&text);
    let get = |k: &str| {
        headers
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.trim().to_string())
    };

    let regist_key_hex =
        get("PS5-RegistKey").context("registration response has no PS5-RegistKey")?;
    let regist_key_bytes = http::parse_hex(&regist_key_hex)?;
    if regist_key_bytes.len() > 16 {
        bail!("PS5-RegistKey too long");
    }
    let mut regist_key = [0u8; 16];
    regist_key[..regist_key_bytes.len()].copy_from_slice(&regist_key_bytes);
    let morning: [u8; 16] =
        http::parse_hex(&get("RP-Key").context("registration response has no RP-Key")?)?
            .try_into()
            .map_err(|_| anyhow!("RP-Key has the wrong size"))?;

    Ok(Registered {
        creds: Credentials {
            regist_key,
            morning,
        },
        nickname: get("PS5-Nickname").unwrap_or_default(),
        mac: get("PS5-Mac").unwrap_or_default(),
    })
}

/// Sends the discovery WAKEUP packet that brings a PS5 out of rest mode.
pub fn wakeup(ip: IpAddr, creds: &Credentials) -> Result<()> {
    // the credential is the regist key (itself a hex string) read as a hex number
    let key_str = String::from_utf8_lossy(creds.regist_key_trimmed()).to_string();
    let credential = u64::from_str_radix(&key_str, 16)
        .map_err(|_| anyhow!("regist key {key_str:?} can't be used for wakeup"))?;
    let packet = format!(
        "WAKEUP * HTTP/1.1\n\
         client-type:vr\n\
         auth-type:R\n\
         model:w\n\
         app-type:r\n\
         user-credential:{credential}\n\
         device-discovery-protocol-version:00030010\n"
    );
    let udp = UdpSocket::bind(if ip.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    udp.send_to(packet.as_bytes(), SocketAddr::new(ip, 9302))?;
    Ok(())
}

/// Asks the console for its status. Returns e.g. "200 Ok" (on) or "620 Server Standby" (rest mode).
pub fn probe(ip: IpAddr) -> Result<String> {
    let udp = UdpSocket::bind(if ip.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" })?;
    udp.set_read_timeout(Some(Duration::from_millis(1500)))?;
    udp.send_to(
        b"SRCH * HTTP/1.1\ndevice-discovery-protocol-version:00030010\n",
        SocketAddr::new(ip, 9302),
    )?;
    let mut buf = [0u8; 1024];
    let (n, _) = udp
        .recv_from(&mut buf)
        .context("no discovery answer from the console")?;
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    let status = text
        .lines()
        .next()
        .unwrap_or("")
        .trim_start_matches("HTTP/1.1 ")
        .to_string();
    Ok(status)
}

/// A console that answered the network search.
#[derive(Clone, Debug, PartialEq)]
pub struct FoundConsole {
    pub ip: IpAddr,
    pub name: String,
    /// true = running, false = rest mode
    pub awake: bool,
    /// The console's MAC address as 12 upper case hex digits (`host-id`); empty if unknown.
    pub host_id: String,
}

/// Normalizes a MAC / host id ("aa:bb:cc:dd:ee:ff", "AABBCCDDEEFF") to 12 upper case hex digits.
pub fn normalize_host_id(s: &str) -> String {
    s.chars()
        .filter(char::is_ascii_hexdigit)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Parses a discovery answer (`HTTP/1.1 200 Ok` + headers). Returns `None` for anything
/// that isn't a PS5.
pub fn parse_discovery_reply(text: &str, from: IpAddr) -> Option<FoundConsole> {
    let headers = http::parse_headers(text);
    let get = |k: &str| {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(k))
            .map(|(_, v)| v.trim().to_string())
    };
    if get("host-type").as_deref() != Some("PS5") {
        return None;
    }
    let status = text.lines().next().unwrap_or("");
    Some(FoundConsole {
        ip: from,
        name: get("host-name").unwrap_or_else(|| "PS5".into()),
        awake: status.contains(" 200 "),
        host_id: get("host-id")
            .map(|h| normalize_host_id(&h))
            .unwrap_or_default(),
    })
}

/// The local IPv4 address used to reach the LAN (no packet is sent).
fn local_ipv4() -> Option<std::net::Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?; // TEST-NET address: only selects the route
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
        _ => None,
    }
}

/// Searches the local network for PS5 consoles for `wait`.
pub fn discover(wait: Duration) -> Result<Vec<FoundConsole>> {
    discover_until(wait, |_| false)
}

/// Searches until `wait` is over or `done` returns true for the consoles found so far.
fn discover_until(
    wait: Duration,
    done: impl Fn(&[FoundConsole]) -> bool,
) -> Result<Vec<FoundConsole>> {
    let udp = UdpSocket::bind("0.0.0.0:0")?;
    udp.set_broadcast(true)?;
    let query = b"SRCH * HTTP/1.1\ndevice-discovery-protocol-version:00030010\n";
    let mut targets: Vec<std::net::Ipv4Addr> = vec![std::net::Ipv4Addr::BROADCAST];
    if let Some(local) = local_ipv4() {
        // Windows may send the limited broadcast on another adapter; also use the /24 one
        let o = local.octets();
        targets.push(std::net::Ipv4Addr::new(o[0], o[1], o[2], 255));
    }
    let deadline = Instant::now() + wait;
    let mut found: Vec<FoundConsole> = Vec::new();
    let mut next_send = Instant::now();
    let mut buf = [0u8; 1024];
    while Instant::now() < deadline && !done(&found) {
        if Instant::now() >= next_send {
            for t in &targets {
                let _ = udp.send_to(query, SocketAddr::new(IpAddr::V4(*t), 9302));
            }
            next_send = Instant::now() + Duration::from_millis(700);
        }
        let left = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(200));
        if left.is_zero() {
            break;
        }
        udp.set_read_timeout(Some(left))?;
        let Ok((n, from)) = udp.recv_from(&mut buf) else {
            continue;
        };
        let text = String::from_utf8_lossy(&buf[..n]).to_string();
        let Some(console) = parse_discovery_reply(&text, from.ip()) else {
            continue;
        };
        match found.iter_mut().find(|f| f.ip == console.ip) {
            // a console waking up answers "620" first and "200" later
            Some(f) => *f = console,
            None => found.push(console),
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name).then(a.ip.cmp(&b.ip)));
    Ok(found)
}

/// Searches the LAN for the console with this MAC / host id (stops as soon as it answers).
pub fn find_by_host_id(host_id: &str, wait: Duration) -> Result<Option<FoundConsole>> {
    let id = normalize_host_id(host_id);
    if id.is_empty() {
        return Ok(None);
    }
    let found = discover_until(wait, |f| f.iter().any(|c| c.host_id == id))?;
    Ok(found.into_iter().find(|c| c.host_id == id))
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    const REPLY_ON: &str = "HTTP/1.1 200 Ok\nhost-id:a1b2c3d4e5f6\nhost-type:PS5\nhost-name:PS5-123\n\
        host-request-port:997\ndevice-discovery-protocol-version:00030010\nsystem-version:09060000\n";

    #[test]
    fn parses_running_console() {
        let ip: IpAddr = "192.168.1.50".parse().unwrap();
        let c = parse_discovery_reply(REPLY_ON, ip).unwrap();
        assert_eq!(c.ip, ip);
        assert_eq!(c.name, "PS5-123");
        assert!(c.awake);
        assert_eq!(c.host_id, "A1B2C3D4E5F6");
    }

    #[test]
    fn parses_rest_mode_and_ignores_others() {
        let ip: IpAddr = "10.0.0.2".parse().unwrap();
        let standby = REPLY_ON.replace("200 Ok", "620 Server Standby");
        assert!(!parse_discovery_reply(&standby, ip).unwrap().awake);
        let ps4 = REPLY_ON.replace("host-type:PS5", "host-type:PS4");
        assert!(parse_discovery_reply(&ps4, ip).is_none());
        assert!(parse_discovery_reply("garbage", ip).is_none());
    }

    #[test]
    fn host_id_normalizes() {
        assert_eq!(normalize_host_id("a1:b2:c3:d4:e5:f6"), "A1B2C3D4E5F6");
        assert_eq!(normalize_host_id(" A1-B2-C3-D4-E5-F6 "), "A1B2C3D4E5F6");
    }
}

#[cfg(test)]
mod network_tests {
    /// Looks for consoles on the LAN; run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn discover_lan() {
        for c in super::discover(std::time::Duration::from_secs(2)).unwrap() {
            println!(
                "found {} at {} ({})",
                c.name,
                c.ip,
                if c.awake { "on" } else { "rest mode" }
            );
        }
    }
}
